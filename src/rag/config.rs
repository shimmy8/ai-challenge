use anyhow::{bail, Context, Result};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{
    fmt, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

#[cfg(test)]
use std::io::Write;

pub(crate) const DEFAULT_INDEX_FILE: &str = ".fox-index.db";
pub(crate) const DEFAULT_EMBEDDING_CONFIG_FILE: &str = ".fox-embeddings.json";
pub(crate) const DEFAULT_CHUNK_SIZE: usize = 1200;
pub(crate) const DEFAULT_CHUNK_OVERLAP: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum IndexStrategy {
    Fixed,
    Structural,
    All,
}

impl IndexStrategy {
    pub(crate) fn concrete(self) -> Vec<Self> {
        match self {
            Self::All => vec![Self::Fixed, Self::Structural],
            value => vec![value],
        }
    }
}

impl fmt::Display for IndexStrategy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Fixed => "fixed",
            Self::Structural => "structural",
            Self::All => "all",
        })
    }
}

impl FromStr for IndexStrategy {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "fixed" => Ok(Self::Fixed),
            "structural" => Ok(Self::Structural),
            "all" => Ok(Self::All),
            _ => bail!("неизвестная стратегия индексации: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct IndexOptions {
    pub(crate) sources: Vec<PathBuf>,
    pub(crate) strategy: IndexStrategy,
    pub(crate) index_path: PathBuf,
    pub(crate) chunk_size: usize,
    pub(crate) chunk_overlap: usize,
    pub(crate) comparison_output: Option<PathBuf>,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            strategy: IndexStrategy::All,
            index_path: PathBuf::from(DEFAULT_INDEX_FILE),
            chunk_size: DEFAULT_CHUNK_SIZE,
            chunk_overlap: DEFAULT_CHUNK_OVERLAP,
            comparison_output: None,
        }
    }
}

pub(crate) fn parse_index_options(args: &[String]) -> Result<IndexOptions> {
    let mut options = IndexOptions::default();
    let mut index = 0;
    let mut strategy_set = false;
    let mut index_path_set = false;
    let mut chunk_size_set = false;
    let mut chunk_overlap_set = false;
    let mut comparison_output_set = false;

    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .with_context(|| format!("для {flag} требуется значение"))?;
        match flag {
            "--source" => options.sources.push(PathBuf::from(value)),
            "--strategy" if !strategy_set => {
                options.strategy = value.parse()?;
                strategy_set = true;
            }
            "--index-path" if !index_path_set => {
                options.index_path = PathBuf::from(value);
                index_path_set = true;
            }
            "--chunk-size" if !chunk_size_set => {
                options.chunk_size = value
                    .parse()
                    .with_context(|| format!("некорректный размер чанка: {value}"))?;
                chunk_size_set = true;
            }
            "--chunk-overlap" if !chunk_overlap_set => {
                options.chunk_overlap = value
                    .parse()
                    .with_context(|| format!("некорректный overlap: {value}"))?;
                chunk_overlap_set = true;
            }
            "--comparison-output" if !comparison_output_set => {
                options.comparison_output = Some(PathBuf::from(value));
                comparison_output_set = true;
            }
            "--strategy"
            | "--index-path"
            | "--chunk-size"
            | "--chunk-overlap"
            | "--comparison-output" => bail!("аргумент указан более одного раза: {flag}"),
            _ => bail!("неизвестный аргумент режима index: {flag}"),
        }
        index += 2;
    }

    anyhow::ensure!(
        options.chunk_size > 0,
        "размер чанка должен быть больше нуля"
    );
    anyhow::ensure!(
        options.chunk_overlap < options.chunk_size,
        "overlap должен быть меньше размера чанка"
    );
    Ok(options)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmbeddingConfig {
    pub(crate) provider: String,
    pub(crate) endpoint: String,
    pub(crate) model: String,
    pub(crate) dimensions: usize,
    pub(crate) batch_size: usize,
    #[serde(default)]
    pub(crate) api_key: String,
}

impl EmbeddingConfig {
    pub(crate) fn load_from_root(root: &Path) -> Result<Self> {
        Self::load(&root.join(DEFAULT_EMBEDDING_CONFIG_FILE))
    }

    pub(crate) fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).with_context(|| {
            format!(
                "не удалось прочитать {}. Создайте файл по образцу fox-embeddings.example.json",
                path.display()
            )
        })?;
        let config: Self = serde_json::from_str(&raw).with_context(|| {
            format!(
                "повреждён {}. Используйте fox-embeddings.example.json",
                path.display()
            )
        })?;
        config.validate()?;
        set_private_permissions(path)?;
        Ok(config)
    }

    #[cfg(test)]
    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let raw = serde_json::to_vec_pretty(self)?;
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .with_context(|| format!("не удалось сохранить {}", path.display()))?;
        file.write_all(&raw)?;
        set_private_permissions(path)?;
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.provider == "openai-compatible",
            "неизвестный embedding provider: {}",
            self.provider
        );
        anyhow::ensure!(!self.model.trim().is_empty(), "embedding model не указан");
        anyhow::ensure!(
            self.dimensions > 0,
            "embedding dimensions должны быть больше нуля"
        );
        anyhow::ensure!(
            self.batch_size > 0,
            "embedding batch_size должен быть больше нуля"
        );
        let endpoint = Url::parse(&self.endpoint).context("некорректный embedding endpoint")?;
        anyhow::ensure!(
            matches!(endpoint.scheme(), "http" | "https"),
            "embedding endpoint должен использовать HTTP(S)"
        );
        anyhow::ensure!(
            endpoint.host().is_some(),
            "embedding endpoint не содержит host"
        );
        if endpoint.host_str() == Some("api.openai.com") {
            anyhow::ensure!(
                !self.api_key.trim().is_empty(),
                "для OpenAI embedding endpoint требуется api_key"
            );
        }
        Ok(())
    }

    pub(crate) fn endpoint_origin(&self) -> Result<String> {
        let endpoint = Url::parse(&self.endpoint)?;
        let host = endpoint.host_str().unwrap_or_default();
        let port = endpoint
            .port()
            .map(|value| format!(":{value}"))
            .unwrap_or_default();
        Ok(format!("{}://{host}{port}", endpoint.scheme()))
    }
}

fn set_private_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(endpoint: &str, api_key: &str) -> EmbeddingConfig {
        EmbeddingConfig {
            provider: "openai-compatible".to_owned(),
            endpoint: endpoint.to_owned(),
            model: "embedding-model".to_owned(),
            dimensions: 3,
            batch_size: 2,
            api_key: api_key.to_owned(),
        }
    }

    #[test]
    fn embedding_config_round_trips_privately() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(DEFAULT_EMBEDDING_CONFIG_FILE);
        config("http://127.0.0.1:11434/v1/embeddings", "")
            .save(&path)
            .unwrap();
        assert_eq!(EmbeddingConfig::load(&path).unwrap().dimensions, 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn embedding_config_rejects_missing_unknown_and_invalid_values() {
        let directory = tempfile::tempdir().unwrap();
        assert!(EmbeddingConfig::load_from_root(directory.path()).is_err());
        let mut value = config("https://api.openai.com/v1/embeddings", "");
        assert!(value.validate().is_err());
        value.api_key = "fake".to_owned();
        value.dimensions = 0;
        assert!(value.validate().is_err());
        value.dimensions = 3;
        value.batch_size = 0;
        assert!(value.validate().is_err());
        value.batch_size = 1;
        value.provider = "unknown".to_owned();
        assert!(value.validate().is_err());
        value.provider = "openai-compatible".to_owned();
        value.endpoint = "file:///tmp/model".to_owned();
        assert!(value.validate().is_err());
    }

    #[test]
    fn embedding_config_denies_unknown_fields() {
        let raw = r#"{"provider":"openai-compatible","endpoint":"http://localhost:1/embed","model":"m","dimensions":3,"batch_size":2,"api_key":"","extra":true}"#;
        assert!(serde_json::from_str::<EmbeddingConfig>(raw).is_err());
    }
}
