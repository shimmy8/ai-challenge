#![allow(unused_imports)]
use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use console::{style, Key, Term};
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, Select};
use reqwest::{Client, StatusCode};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
pub(crate) fn default_context_messages() -> usize {
    10
}

pub(crate) const CONFIG_FILE: &str = ".fox-llm.json";
pub(crate) const MODES_FILE: &str = "fox-modes.json";
pub(crate) const SESSIONS_FILE: &str = ".fox-sessions.db";
pub(crate) const SCHEDULER_FILE: &str = ".fox-scheduler.db";
pub(crate) const METRICS_LOG_FILE: &str = "fox-metrics.log";
pub(crate) const OPENAI_KEYS_URL: &str = "https://platform.openai.com/api-keys";
pub(crate) const CLAUDE_KEYS_URL: &str = "https://console.anthropic.com/settings/keys";
pub(crate) const OPENAI_MODELS_URL: &str = "https://api.openai.com/v1/models";
pub(crate) const CLAUDE_MODELS_URL: &str = "https://api.anthropic.com/v1/models";
pub(crate) const OLLAMA_RESPONSES_URL: &str = "http://127.0.0.1:11434/v1/responses";
pub(crate) const OLLAMA_MODELS_URL: &str = "http://127.0.0.1:11434/v1/models";
pub(crate) const COMMANDS: &[(&str, &str)] = &[
    ("/provider", "сменить провайдера"),
    ("/endpoint", "настроить адрес Ollama Remote"),
    ("/model", "выбрать модель текущего провайдера"),
    ("/mode", "выбрать или создать режим ответа"),
    ("/compression", "выбрать стратегию управления контекстом"),
    ("/temperature", "изменить температуру ответов"),
    ("/mcp", "подключить MCP-сервер и выбрать инструменты"),
    ("/rag", "включить, выключить или проверить RAG"),
    ("/profile", "выбрать профиль персонализации"),
    ("/task", "управлять текущей задачей"),
    ("/remember", "сохранить долговременный факт"),
    ("/invariant", "управлять глобальными инвариантами"),
    ("/forget", "удалить долговременный факт"),
    ("/memory", "показать три слоя памяти"),
    ("/new", "начать новую сессию"),
    ("/sessions", "открыть сохранённую сессию"),
    ("/help", "показать подсказку"),
    ("/quit", "выйти"),
];
pub(crate) const BRANCHING_COMMANDS: &[(&str, &str)] = &[
    ("/checkpoint", "сохранить точку и ожидать новую ветку"),
    ("/load", "вернуться к checkpoint и ожидать новую ветку"),
    ("/switch", "выбрать активную ветку"),
    ("/branches", "показать ветки диалога"),
];

pub(crate) const FOX: &str = r#"
  ▓▓▓▓▓                          ▓▓▓▓▓  
▓▓▓▓▓▓▓▓▓                      ▓▓▓▓▓▓▓▓▓
▓▓▓▓▓▓▓▓▓▓▓                  ▓▓▓▓▓▓▓▓▓▓▓
▓▓▓▓ ░░▓▓▓▓▓▓              ▓▓▓▓▓▓░░ ▓▓▓▓
▓▓▓▓ ░░░░▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░ ▓▓▓▓
▓▓▓▓ ░░░░░░▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░ ▓▓▓▓
  ▓▓▓▓▓░░▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░▓▓▓▓▓  
    ▒▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▒    
    ▒▒▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▒▒    
  ▒▒▒▒▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▒▒▒▒  
  ▒▒▒▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▒▒  
▒▒▒▒▓▓▓▓▓░░  ███▓▓▓▓▓▓▓▓░  ██░░▓▓▓▓▓▓▒▒▒
▒▒▒▓▓▓▓▓▓▓▓    ░▓▓▓▓▓▓▓▓░    ▓▓▓▓▓▓▓▓▓▒▒
████▓▓▓▓▓▓▓▓▓▓▓▓▒▒▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓████
█████████▓▓▓▓▓▓▓▒▒▓▓▓▓▓▓▓▓▓▓▓▓▓█████████
    █████████▒▒▒▓▓▓▓▓▓▓▓▓▓▓██████████   
       ██████▒▒▓▓▓▓▓▓▓▓▓▓▓▓███████      
          ███▒▒▓▓▓    ▓▓▓▓▓████         
             ▒▒▒▓▓    ▓▓▓▓▓             
                ▒▒▓▓▓▓▓▓                
                ▒▒▓▓▓▓▓▓       
"#;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Provider {
    Openai,
    Claude,
    Ollama,
    #[serde(rename = "ollama_remote")]
    OllamaRemote,
}

#[derive(Clone, Copy)]
pub(crate) enum AuthMethod {
    CreateInWeb,
    ExistingKey,
}

impl Provider {
    pub(crate) fn all() -> [Self; 4] {
        [Self::Openai, Self::Claude, Self::Ollama, Self::OllamaRemote]
    }
    pub(crate) fn requires_api_key(self) -> bool {
        !matches!(self, Self::Ollama | Self::OllamaRemote)
    }
    pub(crate) fn key_url(self) -> Option<&'static str> {
        match self {
            Self::Openai => Some(OPENAI_KEYS_URL),
            Self::Claude => Some(CLAUDE_KEYS_URL),
            Self::Ollama | Self::OllamaRemote => None,
        }
    }

    pub(crate) fn is_ollama(self) -> bool {
        matches!(self, Self::Ollama | Self::OllamaRemote)
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Openai => "OpenAI",
            Self::Claude => "Claude",
            Self::Ollama => "Ollama",
            Self::OllamaRemote => "Ollama Remote",
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) compression_strategy: CompressionStrategy,
    #[serde(default = "default_context_messages", alias = "summary_messages")]
    pub(crate) context_messages: usize,
    pub(crate) last_provider: Option<Provider>,
    #[serde(default)]
    pub(crate) last_mode: Option<String>,
    pub(crate) providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub(crate) mcp: McpConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpConfig {
    #[serde(default)]
    pub(crate) servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpServerConfig {
    pub(crate) id: String,
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) disabled_tools: Vec<String>,
}

impl McpConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        for server in &self.servers {
            validate_mcp_server_id(&server.id)?;
            anyhow::ensure!(
                ids.insert(server.id.as_str()),
                "повторяющийся ID MCP-сервера: {}",
                server.id
            );
        }
        Ok(())
    }
}

pub(crate) fn validate_mcp_server_id(value: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty(), "ID MCP-сервера не должен быть пустым");
    anyhow::ensure!(
        !value.contains("__"),
        "ID MCP-сервера не должен содержать __"
    );
    anyhow::ensure!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
        "ID MCP-сервера может содержать только строчные ASCII-буквы, цифры и _"
    );
    Ok(())
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ModesConfig {
    #[serde(default)]
    pub(crate) modes: Vec<ResponseMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResponseMode {
    pub(crate) name: String,
    pub(crate) instructions: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ProviderConfig {
    pub(crate) provider: Provider,
    pub(crate) api_key: Option<String>,
    pub(crate) model: String,
    #[serde(default = "default_temperature")]
    pub(crate) temperature: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LegacyConfig {
    pub(crate) last_provider: Option<Provider>,
    pub(crate) openai_api_key: Option<String>,
    pub(crate) claude_api_key: Option<String>,
    #[serde(default = "default_openai_model")]
    pub(crate) openai_model: String,
    #[serde(default = "default_claude_model")]
    pub(crate) claude_model: String,
}

fn default_openai_model() -> String {
    "gpt-5.6-luna".into()
}
fn default_claude_model() -> String {
    "claude-sonnet-5".into()
}
fn default_ollama_model() -> String {
    "qwen3.5:4b".into()
}
fn default_ollama_remote_model() -> String {
    "qwen3:1.7b".into()
}
pub(crate) fn default_temperature() -> f64 {
    1.0
}

fn default_provider_config(provider: Provider) -> ProviderConfig {
    let model = match provider {
        Provider::Openai => default_openai_model(),
        Provider::Claude => default_claude_model(),
        Provider::Ollama => default_ollama_model(),
        Provider::OllamaRemote => default_ollama_remote_model(),
    };
    ProviderConfig {
        provider,
        api_key: None,
        model,
        temperature: default_temperature(),
        endpoint: None,
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            compression_strategy: CompressionStrategy::Summary,
            context_messages: default_context_messages(),
            last_provider: None,
            last_mode: None,
            mcp: McpConfig::default(),
            providers: Provider::all()
                .into_iter()
                .map(default_provider_config)
                .collect(),
        }
    }
}

impl Config {
    pub(crate) fn load_without_mcp_registry(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        let mut value: Value = serde_json::from_str(&raw).context("повреждён файл конфигурации")?;
        if value.get("providers").is_some() {
            value["mcp"] = json!({"servers": []});
            let mut config: Self =
                serde_json::from_value(value).context("повреждён файл конфигурации")?;
            anyhow::ensure!(config.context_messages <= 1000, "размер окна вне диапазона");
            config.ensure_provider_defaults();
            config.normalize_remote_endpoint()?;
            return Ok(config);
        }
        Self::load(path)
    }

    pub(crate) fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        let value: Value = serde_json::from_str(&raw).context("повреждён файл конфигурации")?;
        if value.get("providers").is_some() {
            let mut config: Self =
                serde_json::from_value(value).context("повреждён файл конфигурации")?;
            anyhow::ensure!(config.context_messages <= 1000, "размер окна вне диапазона");
            config.mcp.validate()?;
            let providers_changed = config.ensure_provider_defaults();
            let endpoint_changed = config.normalize_remote_endpoint()?;
            if providers_changed || endpoint_changed {
                config
                    .save(path)
                    .context("не удалось обновить список провайдеров")?;
            }
            return Ok(config);
        }

        let legacy: LegacyConfig =
            serde_json::from_value(value).context("повреждён старый файл конфигурации")?;
        let config = Self {
            compression_strategy: CompressionStrategy::Summary,
            context_messages: default_context_messages(),
            last_provider: legacy.last_provider,
            last_mode: None,
            mcp: McpConfig::default(),
            providers: vec![
                ProviderConfig {
                    provider: Provider::Openai,
                    api_key: legacy.openai_api_key,
                    model: legacy.openai_model,
                    temperature: default_temperature(),
                    endpoint: None,
                },
                ProviderConfig {
                    provider: Provider::Claude,
                    api_key: legacy.claude_api_key,
                    model: legacy.claude_model,
                    temperature: default_temperature(),
                    endpoint: None,
                },
                default_provider_config(Provider::Ollama),
                default_provider_config(Provider::OllamaRemote),
            ],
        };
        config
            .save(path)
            .context("не удалось обновить конфигурацию")?;
        Ok(config)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        self.mcp.validate()?;
        if let Some(endpoint) = self.endpoint(Provider::OllamaRemote) {
            validate_ollama_remote_endpoint(endpoint)?;
        }
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub(crate) fn key(&self, provider: Provider) -> Option<&str> {
        self.provider(provider)
            .and_then(|config| config.api_key.as_deref())
            .filter(|key| !key.trim().is_empty())
    }

    pub(crate) fn set_key(&mut self, provider: Provider, key: String) {
        if let Some(config) = self
            .providers
            .iter_mut()
            .find(|item| item.provider == provider)
        {
            config.api_key = Some(key);
        }
    }

    pub(crate) fn model(&self, provider: Provider) -> Result<&str> {
        self.provider(provider)
            .map(|config| config.model.as_str())
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| anyhow!("для {provider} не указана модель"))
    }

    pub(crate) fn temperature(&self, provider: Provider) -> Result<f64> {
        self.provider(provider)
            .map(|config| config.temperature)
            .ok_or_else(|| anyhow!("не найдена конфигурация для {provider}"))
    }

    pub(crate) fn set_model(&mut self, provider: Provider, model: String) -> Result<()> {
        let config = self
            .providers
            .iter_mut()
            .find(|item| item.provider == provider)
            .ok_or_else(|| anyhow!("не найдена конфигурация для {provider}"))?;
        config.model = model;
        Ok(())
    }

    pub(crate) fn set_temperature(&mut self, provider: Provider, temperature: f64) -> Result<()> {
        let config = self
            .providers
            .iter_mut()
            .find(|item| item.provider == provider)
            .ok_or_else(|| anyhow!("не найдена конфигурация для {provider}"))?;
        config.temperature = temperature;
        Ok(())
    }

    pub(crate) fn endpoint(&self, provider: Provider) -> Option<&str> {
        self.provider(provider)
            .and_then(|config| config.endpoint.as_deref())
            .filter(|endpoint| !endpoint.trim().is_empty())
    }

    pub(crate) fn set_ollama_remote_endpoint(&mut self, value: &str) -> Result<()> {
        let endpoint = normalize_ollama_remote_endpoint(value)?;
        let config = self
            .providers
            .iter_mut()
            .find(|item| item.provider == Provider::OllamaRemote)
            .ok_or_else(|| anyhow!("не найдена конфигурация для Ollama Remote"))?;
        config.endpoint = Some(endpoint);
        Ok(())
    }

    pub(crate) fn provider(&self, provider: Provider) -> Option<&ProviderConfig> {
        self.providers.iter().find(|item| item.provider == provider)
    }

    fn ensure_provider_defaults(&mut self) -> bool {
        let mut changed = false;
        for provider in Provider::all() {
            if self.provider(provider).is_none() {
                self.providers.push(default_provider_config(provider));
                changed = true;
            }
        }
        changed
    }

    fn normalize_remote_endpoint(&mut self) -> Result<bool> {
        let Some(config) = self
            .providers
            .iter_mut()
            .find(|item| item.provider == Provider::OllamaRemote)
        else {
            return Ok(false);
        };
        let Some(value) = config.endpoint.as_deref() else {
            return Ok(false);
        };
        let normalized = normalize_ollama_remote_endpoint(value)?;
        if normalized == value {
            return Ok(false);
        }
        config.endpoint = Some(normalized);
        Ok(true)
    }
}

pub(crate) fn normalize_ollama_remote_endpoint(value: &str) -> Result<String> {
    let value = value.trim();
    anyhow::ensure!(
        !value.is_empty(),
        "endpoint Ollama Remote не может быть пустым"
    );
    let mut url = reqwest::Url::parse(value)
        .context("endpoint Ollama Remote должен быть корректным HTTP(S) URL")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "endpoint Ollama Remote должен использовать http или https"
    );
    anyhow::ensure!(
        url.host_str().is_some(),
        "в endpoint Ollama Remote нет адреса сервера"
    );
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "endpoint Ollama Remote не должен содержать credentials"
    );
    anyhow::ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "endpoint Ollama Remote не должен содержать query или fragment"
    );

    let path = url.path().trim_end_matches('/');
    let normalized_path = if path.is_empty() {
        "/v1".to_owned()
    } else if path.ends_with("/v1") {
        path.to_owned()
    } else {
        format!("{path}/v1")
    };
    url.set_path(&normalized_path);
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

pub(crate) fn validate_ollama_remote_endpoint(value: &str) -> Result<()> {
    normalize_ollama_remote_endpoint(value).map(|_| ())
}

impl ModesConfig {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("повреждён файл режимов {}", path.display()))
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        let raw = serde_json::to_vec_pretty(self)?;
        fs::write(path, raw).with_context(|| format!("не удалось сохранить {}", path.display()))
    }
}
