use super::ReviewOptions;
use anyhow::{bail, Context, Result};
use std::{env, fmt};

pub(crate) const DEFAULT_MAX_FILES: usize = 50;
pub(crate) const DEFAULT_MAX_DIFF_CHARS: usize = 300_000;
pub(crate) const DEFAULT_MAX_FILE_CHARS: usize = 40_000;
pub(crate) const DEFAULT_BATCH_CHARS: usize = 60_000;
pub(crate) const DEFAULT_MAX_FINDINGS: usize = 30;
pub(crate) const DEFAULT_RAG_CONTEXT_CHARS: usize = 6_000;

#[derive(Clone)]
pub(crate) struct ReviewSettings {
    pub(crate) github_token: String,
    pub(crate) api_key: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) embedding_model: String,
    pub(crate) max_files: usize,
    pub(crate) max_diff_chars: usize,
    pub(crate) max_file_chars: usize,
    pub(crate) batch_chars: usize,
    pub(crate) max_findings: usize,
    pub(crate) rag_context_chars: usize,
}

impl fmt::Debug for ReviewSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReviewSettings")
            .field("github_token", &"[REDACTED]")
            .field("api_key", &"[REDACTED]")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("embedding_model", &self.embedding_model)
            .field("max_files", &self.max_files)
            .field("max_diff_chars", &self.max_diff_chars)
            .field("max_file_chars", &self.max_file_chars)
            .field("batch_chars", &self.batch_chars)
            .field("max_findings", &self.max_findings)
            .field("rag_context_chars", &self.rag_context_chars)
            .finish()
    }
}

impl ReviewSettings {
    pub(crate) fn from_environment(options: &ReviewOptions) -> Result<Self> {
        Self::from_source(options, |name| env::var(name).ok())
    }

    fn from_source(options: &ReviewOptions, get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let required = |name: &str| {
            get(name)
                .filter(|value| !value.trim().is_empty())
                .with_context(|| format!("не задана переменная {name}"))
        };
        let provider = options
            .provider
            .clone()
            .or_else(|| get("FOX_REVIEW_PROVIDER"))
            .unwrap_or_else(|| "openai".to_owned());
        if provider != "openai" {
            bail!("review provider должен быть openai");
        }
        let model = options
            .model
            .clone()
            .or_else(|| get("FOX_REVIEW_MODEL"))
            .filter(|value| !value.trim().is_empty())
            .context("не задана модель: используйте --model или FOX_REVIEW_MODEL")?;
        let embedding_model = options
            .embedding_model
            .clone()
            .or_else(|| get("FOX_REVIEW_EMBEDDING_MODEL"))
            .filter(|value| !value.trim().is_empty())
            .context(
                "не задана embedding-модель: используйте --embedding-model или FOX_REVIEW_EMBEDDING_MODEL",
            )?;
        let parse_limit = |cli: Option<usize>, name: &str, default: usize| -> Result<usize> {
            if let Some(value) = cli {
                return Ok(value);
            }
            let Some(raw) = get(name) else {
                return Ok(default);
            };
            let value = raw
                .parse::<usize>()
                .with_context(|| format!("{name} должен быть положительным числом"))?;
            anyhow::ensure!(value > 0, "{name} должен быть больше нуля");
            Ok(value)
        };
        let settings = Self {
            github_token: required("GITHUB_TOKEN")?,
            api_key: required("OPENAI_API_KEY")?,
            provider,
            model,
            embedding_model,
            max_files: parse_limit(options.max_files, "FOX_REVIEW_MAX_FILES", DEFAULT_MAX_FILES)?,
            max_diff_chars: parse_limit(
                options.max_diff_chars,
                "FOX_REVIEW_MAX_DIFF_CHARS",
                DEFAULT_MAX_DIFF_CHARS,
            )?,
            max_file_chars: parse_limit(
                options.max_file_chars,
                "FOX_REVIEW_MAX_FILE_CHARS",
                DEFAULT_MAX_FILE_CHARS,
            )?,
            batch_chars: parse_limit(
                options.batch_chars,
                "FOX_REVIEW_BATCH_CHARS",
                DEFAULT_BATCH_CHARS,
            )?,
            max_findings: parse_limit(
                options.max_findings,
                "FOX_REVIEW_MAX_FINDINGS",
                DEFAULT_MAX_FINDINGS,
            )?,
            rag_context_chars: DEFAULT_RAG_CONTEXT_CHARS,
        };
        anyhow::ensure!(
            settings.max_file_chars <= settings.max_diff_chars,
            "лимит одного файла не может превышать общий лимит diff"
        );
        anyhow::ensure!(
            settings.max_file_chars <= settings.batch_chars,
            "лимит одного файла не может превышать лимит одного batch"
        );
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn options() -> ReviewOptions {
        super::super::parse_review_options(
            &[
                "--repository",
                "owner/repo",
                "--pull-request",
                "7",
                "--output",
                "review.md",
                "--json-output",
                "review.json",
            ]
            .map(str::to_owned),
        )
        .unwrap()
    }

    #[test]
    fn settings_use_cli_then_environment_then_defaults() {
        let mut options = options();
        options.model = Some("cli-model".to_owned());
        let values = HashMap::from([
            ("GITHUB_TOKEN", "github-secret"),
            ("OPENAI_API_KEY", "openai-secret"),
            ("FOX_REVIEW_MODEL", "env-model"),
            ("FOX_REVIEW_EMBEDDING_MODEL", "embed-model"),
        ]);
        let settings = ReviewSettings::from_source(&options, |name| {
            values.get(name).map(|value| (*value).to_owned())
        })
        .unwrap();

        assert_eq!(settings.model, "cli-model");
        assert_eq!(settings.max_files, 50);
        assert_eq!(settings.max_diff_chars, 300_000);
    }

    #[test]
    fn cli_limit_overrides_environment() {
        let mut options = options();
        options.max_files = Some(9);
        let values = HashMap::from([
            ("GITHUB_TOKEN", "github-secret"),
            ("OPENAI_API_KEY", "openai-secret"),
            ("FOX_REVIEW_MODEL", "review-model"),
            ("FOX_REVIEW_EMBEDDING_MODEL", "embed-model"),
            ("FOX_REVIEW_MAX_FILES", "77"),
        ]);
        let settings =
            ReviewSettings::from_source(&options, |name| values.get(name).map(|v| (*v).to_owned()))
                .unwrap();
        assert_eq!(settings.max_files, 9);
    }

    #[test]
    fn settings_debug_and_errors_do_not_expose_secrets() {
        let values = HashMap::from([
            ("GITHUB_TOKEN", "github-secret"),
            ("OPENAI_API_KEY", "openai-secret"),
            ("FOX_REVIEW_MODEL", "review-model"),
            ("FOX_REVIEW_EMBEDDING_MODEL", "embed-model"),
        ]);
        let settings = ReviewSettings::from_source(&options(), |name| {
            values.get(name).map(|value| (*value).to_owned())
        })
        .unwrap();
        let debug = format!("{settings:?}");

        assert!(!debug.contains("github-secret"));
        assert!(!debug.contains("openai-secret"));
        assert!(debug.contains("[REDACTED]"));
    }
}
