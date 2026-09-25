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
pub(crate) const COMMANDS: &[(&str, &str)] = &[
    ("/provider", "сменить провайдера"),
    ("/model", "выбрать модель текущего провайдера"),
    ("/mode", "выбрать или создать режим ответа"),
    ("/compression", "выбрать стратегию управления контекстом"),
    ("/temperature", "изменить температуру ответов"),
    ("/mcp", "подключить MCP-сервер и выбрать инструменты"),
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
}

#[derive(Clone, Copy)]
pub(crate) enum AuthMethod {
    CreateInWeb,
    ExistingKey,
}

impl Provider {
    pub(crate) fn all() -> [Self; 2] {
        [Self::Openai, Self::Claude]
    }
    pub(crate) fn key_url(self) -> &'static str {
        match self {
            Self::Openai => OPENAI_KEYS_URL,
            Self::Claude => CLAUDE_KEYS_URL,
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Openai => "OpenAI",
            Self::Claude => "Claude",
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
pub(crate) fn default_temperature() -> f64 {
    1.0
}

impl Default for Config {
    fn default() -> Self {
        Self {
            compression_strategy: CompressionStrategy::Summary,
            context_messages: default_context_messages(),
            last_provider: None,
            last_mode: None,
            mcp: McpConfig::default(),
            providers: vec![
                ProviderConfig {
                    provider: Provider::Openai,
                    api_key: None,
                    model: default_openai_model(),
                    temperature: default_temperature(),
                },
                ProviderConfig {
                    provider: Provider::Claude,
                    api_key: None,
                    model: default_claude_model(),
                    temperature: default_temperature(),
                },
            ],
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
            let config: Self =
                serde_json::from_value(value).context("повреждён файл конфигурации")?;
            anyhow::ensure!(config.context_messages <= 1000, "размер окна вне диапазона");
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
            let config: Self =
                serde_json::from_value(value).context("повреждён файл конфигурации")?;
            anyhow::ensure!(config.context_messages <= 1000, "размер окна вне диапазона");
            config.mcp.validate()?;
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
                },
                ProviderConfig {
                    provider: Provider::Claude,
                    api_key: legacy.claude_api_key,
                    model: legacy.claude_model,
                    temperature: default_temperature(),
                },
            ],
        };
        config
            .save(path)
            .context("не удалось обновить конфигурацию")?;
        Ok(config)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        self.mcp.validate()?;
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

    pub(crate) fn provider(&self, provider: Provider) -> Option<&ProviderConfig> {
        self.providers.iter().find(|item| item.provider == provider)
    }
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
