use anyhow::{anyhow, bail, Context, Result};
use console::{style, Key, Term};
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, Select};
use reqwest::{Client, StatusCode};
use rusqlite::{params, Connection};
use rustyline::{
    completion::{Completer, Pair},
    error::ReadlineError,
    highlight::Highlighter,
    hint::Hinter,
    history::DefaultHistory,
    validate::Validator,
    Context as ReadlineContext, Editor, Helper,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

struct ApiAnswer {
    text: String,
    input_tokens: u64,
    output_tokens: u64,
    session_input_tokens: u64,
    session_output_tokens: u64,
}

#[derive(Debug, Serialize)]
struct MetricsLogEntry<'a> {
    timestamp_unix_ms: u128,
    agent_id: usize,
    provider: Provider,
    model: &'a str,
    elapsed_ms: u128,
    request_input_tokens: u64,
    request_output_tokens: u64,
    session_input_tokens: u64,
    session_output_tokens: u64,
}

#[derive(Debug, Clone)]
struct AgentSettings {
    provider: Provider,
    api_key: String,
    model: String,
    temperature: f64,
    instructions: Option<String>,
}

impl AgentSettings {
    fn from_config(
        config: &Config,
        provider: Provider,
        mode: Option<&ResponseMode>,
    ) -> Result<Self> {
        Ok(Self {
            provider,
            api_key: config
                .key(provider)
                .ok_or_else(|| anyhow!("нет ключа {provider}"))?
                .to_owned(),
            model: config.model(provider)?.to_owned(),
            temperature: config.temperature(provider)?,
            instructions: mode
                .map(|mode| mode.instructions.trim())
                .filter(|instructions| !instructions.is_empty())
                .map(str::to_owned),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentStatus {
    Idle,
    Running,
    Completed,
    Failed(String),
}

struct Agent {
    id: usize,
    client: Client,
    settings: AgentSettings,
    history: Vec<Message>,
    persisted_history: Vec<Message>,
    session_input_tokens: u64,
    session_output_tokens: u64,
    status: AgentStatus,
}

impl Agent {
    fn new(id: usize, client: Client, settings: AgentSettings) -> Self {
        Self {
            id,
            client,
            settings,
            history: Vec::new(),
            persisted_history: Vec::new(),
            session_input_tokens: 0,
            session_output_tokens: 0,
            status: AgentStatus::Idle,
        }
    }

    async fn ask(&mut self, input: &str) -> Result<ApiAnswer> {
        self.status = AgentStatus::Running;
        self.history.push(Message {
            role: "user".to_owned(),
            content: input.to_owned(),
        });
        self.persisted_history.push(Message {
            role: "user".to_owned(),
            content: input.to_owned(),
        });

        match send_request(&self.client, &self.settings, &self.history).await {
            Ok(mut answer) => {
                self.session_input_tokens += answer.input_tokens;
                self.session_output_tokens += answer.output_tokens;
                answer.session_input_tokens = self.session_input_tokens;
                answer.session_output_tokens = self.session_output_tokens;
                self.history.push(Message {
                    role: "assistant".to_owned(),
                    content: answer.text.clone(),
                });
                self.persisted_history.push(Message {
                    role: "assistant".to_owned(),
                    content: answer.text.clone(),
                });
                self.status = AgentStatus::Completed;
                Ok(answer)
            }
            Err(error) => {
                self.history.pop();
                self.persisted_history.pop();
                self.status = AgentStatus::Failed(error.to_string());
                Err(error)
            }
        }
    }

    fn reset(&mut self) {
        self.history.clear();
        self.persisted_history.clear();
        self.session_input_tokens = 0;
        self.session_output_tokens = 0;
        self.status = AgentStatus::Idle;
    }

    fn restore(&mut self, messages: Vec<Message>) {
        self.reset();
        if !messages.is_empty() {
            let toon = encode_messages_toon(&messages);
            self.history.push(Message {
                role: "user".to_owned(),
                content: format!(
                    "Ниже приведён контекст предыдущей части нашего диалога в формате TOON. Продолжай разговор с учётом этого контекста.\n\n{toon}"
                ),
            });
            self.persisted_history = messages;
        }
    }

    fn reconfigure(&mut self, settings: AgentSettings) {
        self.settings = settings;
        self.reset();
    }
}

struct AgentPool {
    agents: Vec<Agent>,
}

struct AgentRunResult {
    agent_id: usize,
    elapsed: std::time::Duration,
    result: Result<ApiAnswer>,
}

impl AgentPool {
    fn new(agent_count: usize, client: Client, settings: AgentSettings) -> Self {
        let agents = (1..=agent_count)
            .map(|id| Agent::new(id, client.clone(), settings.clone()))
            .collect();
        Self { agents }
    }

    async fn ask_all(&mut self, input: &str) -> Vec<AgentRunResult> {
        let mut tasks = tokio::task::JoinSet::new();
        for mut agent in self.agents.drain(..) {
            let input = input.to_owned();
            tasks.spawn(async move {
                let started = Instant::now();
                let result = agent.ask(&input).await;
                let run = AgentRunResult {
                    agent_id: agent.id,
                    elapsed: started.elapsed(),
                    result,
                };
                (agent, run)
            });
        }

        let mut results = Vec::new();
        while let Some(task) = tasks.join_next().await {
            match task {
                Ok((agent, run)) => {
                    self.agents.push(agent);
                    results.push(run);
                }
                Err(error) => results.push(AgentRunResult {
                    agent_id: 0,
                    elapsed: std::time::Duration::ZERO,
                    result: Err(anyhow!("задача агента аварийно завершилась: {error}")),
                }),
            }
        }
        self.agents.sort_by_key(|agent| agent.id);
        results.sort_by_key(|run| run.agent_id);
        results
    }

    fn reset(&mut self) {
        self.agents.iter_mut().for_each(Agent::reset);
    }

    fn reconfigure(&mut self, settings: AgentSettings) {
        self.agents
            .iter_mut()
            .for_each(|agent| agent.reconfigure(settings.clone()));
    }

    fn restore(&mut self, messages: Vec<Message>) {
        self.agents
            .iter_mut()
            .for_each(|agent| agent.restore(messages.clone()));
    }

    fn persisted_history(&self) -> &[Message] {
        self.agents
            .first()
            .map(|agent| agent.persisted_history.as_slice())
            .unwrap_or_default()
    }
}

const CONFIG_FILE: &str = ".fox-llm.json";
const MODES_FILE: &str = "fox-modes.json";
const SESSIONS_FILE: &str = ".fox-sessions.db";
const METRICS_LOG_FILE: &str = "fox-metrics.log";
const OPENAI_KEYS_URL: &str = "https://platform.openai.com/api-keys";
const CLAUDE_KEYS_URL: &str = "https://console.anthropic.com/settings/keys";
const OPENAI_MODELS_URL: &str = "https://api.openai.com/v1/models";
const CLAUDE_MODELS_URL: &str = "https://api.anthropic.com/v1/models";
const COMMANDS: &[(&str, &str)] = &[
    ("/provider", "сменить провайдера"),
    ("/model", "выбрать модель текущего провайдера"),
    ("/mode", "выбрать или создать режим ответа"),
    ("/temperature", "изменить температуру ответов"),
    ("/new", "начать новую сессию"),
    ("/sessions", "открыть сохранённую сессию"),
    ("/help", "показать подсказку"),
    ("/quit", "выйти"),
];

const FOX: &str = r#"
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
enum Provider {
    Openai,
    Claude,
}

#[derive(Clone, Copy)]
enum AuthMethod {
    CreateInWeb,
    ExistingKey,
}

impl Provider {
    fn all() -> [Self; 2] {
        [Self::Openai, Self::Claude]
    }
    fn key_url(self) -> &'static str {
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
struct Config {
    last_provider: Option<Provider>,
    #[serde(default)]
    last_mode: Option<String>,
    providers: Vec<ProviderConfig>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ModesConfig {
    #[serde(default)]
    modes: Vec<ResponseMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseMode {
    name: String,
    instructions: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProviderConfig {
    provider: Provider,
    api_key: Option<String>,
    model: String,
    #[serde(default = "default_temperature")]
    temperature: f64,
}

#[derive(Debug, Deserialize)]
struct LegacyConfig {
    last_provider: Option<Provider>,
    openai_api_key: Option<String>,
    claude_api_key: Option<String>,
    #[serde(default = "default_openai_model")]
    openai_model: String,
    #[serde(default = "default_claude_model")]
    claude_model: String,
}

fn default_openai_model() -> String {
    "gpt-5.6-luna".into()
}
fn default_claude_model() -> String {
    "claude-sonnet-5".into()
}
fn default_temperature() -> f64 {
    1.0
}

impl Default for Config {
    fn default() -> Self {
        Self {
            last_provider: None,
            last_mode: None,
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
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        let value: Value = serde_json::from_str(&raw).context("повреждён файл конфигурации")?;
        if value.get("providers").is_some() {
            return serde_json::from_value(value).context("повреждён файл конфигурации");
        }

        let legacy: LegacyConfig =
            serde_json::from_value(value).context("повреждён старый файл конфигурации")?;
        let config = Self {
            last_provider: legacy.last_provider,
            last_mode: None,
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

    fn save(&self, path: &Path) -> Result<()> {
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

    fn key(&self, provider: Provider) -> Option<&str> {
        self.provider(provider)
            .and_then(|config| config.api_key.as_deref())
            .filter(|key| !key.trim().is_empty())
    }

    fn set_key(&mut self, provider: Provider, key: String) {
        if let Some(config) = self
            .providers
            .iter_mut()
            .find(|item| item.provider == provider)
        {
            config.api_key = Some(key);
        }
    }

    fn model(&self, provider: Provider) -> Result<&str> {
        self.provider(provider)
            .map(|config| config.model.as_str())
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| anyhow!("для {provider} не указана модель"))
    }

    fn temperature(&self, provider: Provider) -> Result<f64> {
        self.provider(provider)
            .map(|config| config.temperature)
            .ok_or_else(|| anyhow!("не найдена конфигурация для {provider}"))
    }

    fn set_model(&mut self, provider: Provider, model: String) -> Result<()> {
        let config = self
            .providers
            .iter_mut()
            .find(|item| item.provider == provider)
            .ok_or_else(|| anyhow!("не найдена конфигурация для {provider}"))?;
        config.model = model;
        Ok(())
    }

    fn set_temperature(&mut self, provider: Provider, temperature: f64) -> Result<()> {
        let config = self
            .providers
            .iter_mut()
            .find(|item| item.provider == provider)
            .ok_or_else(|| anyhow!("не найдена конфигурация для {provider}"))?;
        config.temperature = temperature;
        Ok(())
    }

    fn provider(&self, provider: Provider) -> Option<&ProviderConfig> {
        self.providers.iter().find(|item| item.provider == provider)
    }
}

impl ModesConfig {
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("повреждён файл режимов {}", path.display()))
    }

    fn save(&self, path: &Path) -> Result<()> {
        let raw = serde_json::to_vec_pretty(self)?;
        fs::write(path, raw).with_context(|| format!("не удалось сохранить {}", path.display()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Message {
    role: String,
    content: String,
}

struct SavedSession {
    id: i64,
    title: String,
    provider: Provider,
    model: String,
    mode: Option<String>,
    temperature: f64,
    messages: Vec<Message>,
}

struct SessionStore {
    connection: Connection,
}

impl SessionStore {
    fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("не удалось открыть базу сессий {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS sessions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 title TEXT NOT NULL,
                 provider TEXT NOT NULL,
                 model TEXT NOT NULL,
                 mode TEXT,
                 temperature REAL NOT NULL,
                 history_toon TEXT NOT NULL,
                 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                 updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );",
        )?;
        Ok(Self { connection })
    }

    fn save(
        &self,
        id: Option<i64>,
        provider: Provider,
        model: &str,
        mode: Option<&str>,
        temperature: f64,
        messages: &[Message],
    ) -> Result<i64> {
        let history = encode_messages_toon(messages);
        let title = messages
            .iter()
            .find(|message| message.role == "user")
            .map(|message| session_title(&message.content))
            .unwrap_or_else(|| "Новая сессия".to_owned());
        let provider = provider_id(provider);
        if let Some(id) = id {
            self.connection.execute(
                "UPDATE sessions SET title = ?1, provider = ?2, model = ?3, mode = ?4,
                 temperature = ?5, history_toon = ?6, updated_at = CURRENT_TIMESTAMP WHERE id = ?7",
                params![title, provider, model, mode, temperature, history, id],
            )?;
            Ok(id)
        } else {
            self.connection.execute(
                "INSERT INTO sessions (title, provider, model, mode, temperature, history_toon)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![title, provider, model, mode, temperature, history],
            )?;
            Ok(self.connection.last_insert_rowid())
        }
    }

    fn list(&self) -> Result<Vec<(i64, String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT id, title, updated_at FROM sessions ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn load(&self, id: i64) -> Result<SavedSession> {
        let row = self.connection.query_row(
            "SELECT id, title, provider, model, mode, temperature, history_toon
             FROM sessions WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )?;
        Ok(SavedSession {
            id: row.0,
            title: row.1,
            provider: parse_provider(&row.2)?,
            model: row.3,
            mode: row.4,
            temperature: row.5,
            messages: decode_messages_toon(&row.6)?,
        })
    }

    fn delete(&self, id: i64) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?
            > 0)
    }
}

fn provider_id(provider: Provider) -> &'static str {
    match provider {
        Provider::Openai => "openai",
        Provider::Claude => "claude",
    }
}

fn parse_provider(value: &str) -> Result<Provider> {
    match value {
        "openai" => Ok(Provider::Openai),
        "claude" => Ok(Provider::Claude),
        _ => bail!("неизвестный провайдер в сохранённой сессии: {value}"),
    }
}

fn session_title(content: &str) -> String {
    let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let title: String = chars.by_ref().take(48).collect();
    if chars.next().is_some() {
        format!("{title}…")
    } else {
        title
    }
}

fn encode_messages_toon(messages: &[Message]) -> String {
    let mut output = format!("messages[{}]{{role,content}}:", messages.len());
    for message in messages {
        let content = serde_json::to_string(&message.content).expect("String always serializes");
        output.push_str(&format!("\n  {},{}", message.role, content));
    }
    output
}

fn decode_messages_toon(input: &str) -> Result<Vec<Message>> {
    let mut lines = input.lines();
    let header = lines.next().context("пустая история TOON")?;
    if !header.starts_with("messages[") || !header.ends_with("{role,content}:") {
        bail!("неверный заголовок истории TOON");
    }
    let declared_count = header
        .strip_prefix("messages[")
        .and_then(|value| value.strip_suffix("]{role,content}:"))
        .context("неверный заголовок истории TOON")?
        .parse::<usize>()
        .context("неверное число сообщений в истории TOON")?;
    let messages = lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (role, content) = line
                .trim_start()
                .split_once(',')
                .context("повреждена строка истории TOON")?;
            if role != "user" && role != "assistant" {
                bail!("неизвестная роль в истории TOON: {role}");
            }
            Ok(Message {
                role: role.to_owned(),
                content: serde_json::from_str(content)
                    .context("повреждено содержимое сообщения TOON")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if messages.len() != declared_count {
        bail!(
            "число сообщений в истории TOON не совпадает: ожидалось {declared_count}, найдено {}",
            messages.len()
        );
    }
    Ok(messages)
}

struct CommandHelper;

impl Helper for CommandHelper {}
impl Highlighter for CommandHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(format!("\x1b[2m{hint}\x1b[0m"))
    }
}
impl Validator for CommandHelper {}

impl Completer for CommandHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _context: &ReadlineContext<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let prefix = &line[..pos];
        if !prefix.starts_with('/') || prefix.contains(char::is_whitespace) {
            return Ok((pos, Vec::new()));
        }
        let candidates = COMMANDS
            .iter()
            .filter(|(command, _)| command.starts_with(prefix))
            .map(|(command, description)| Pair {
                display: format!("{command:<12} {description}"),
                replacement: (*command).to_owned(),
            })
            .collect();
        Ok((0, candidates))
    }
}

impl Hinter for CommandHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _context: &ReadlineContext<'_>) -> Option<String> {
        if pos != line.len() || !line.starts_with('/') || line.contains(char::is_whitespace) {
            return None;
        }
        COMMANDS
            .iter()
            .map(|(command, _)| *command)
            .find(|command| command.starts_with(line) && *command != line)
            .map(|command| command[line.len()..].to_owned())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let dump_metrics = parse_dump_metrics_flag(std::env::args().skip(1))?;
    print_banner();
    let config_path = config_path()?;
    let mut config = Config::load(&config_path)?;
    let modes_path = modes_path()?;
    let mut modes = ModesConfig::load(&modes_path)?;
    let sessions = SessionStore::open(&sessions_path()?)?;
    let metrics_log_path = dump_metrics.then(metrics_path).transpose()?;
    let mut active_session_id = None;
    let mut provider = match config.last_provider {
        Some(saved) => saved,
        None => choose_provider()?,
    };
    authorize_if_needed(&mut config, provider, &config_path)?;
    remember_provider(&mut config, provider, &config_path)?;
    let mut active_mode = config
        .last_mode
        .as_deref()
        .and_then(|name| modes.modes.iter().position(|mode| mode.name == name));
    println!("Введите {} для списка команд.\n", style("/help").yellow());
    if let Some(path) = &metrics_log_path {
        println!(
            "{} {}\n",
            style("Лог метрик:").dim(),
            style(path.display()).cyan()
        );
    }

    let client = Client::builder().user_agent("fox-llm/0.1.0").build()?;
    let initial_mode = active_mode.and_then(|index| modes.modes.get(index));
    let mut agents = AgentPool::new(
        1,
        client.clone(),
        AgentSettings::from_config(&config, provider, initial_mode)?,
    );
    let mut editor = Editor::<CommandHelper, DefaultHistory>::new()?;
    editor.set_helper(Some(CommandHelper));
    loop {
        show_status_bar(
            provider,
            config.model(provider)?,
            mode_name(&modes, active_mode),
            config.temperature(provider)?,
        )?;
        let prompt = format!("{} ", style("Вы ›").green().bold());
        let readline_result = editor.readline(&prompt);
        clear_status_bar()?;
        let input = match readline_result {
            Ok(value) => value.trim().to_owned(),
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => break,
            Err(err) => return Err(err.into()),
        };
        if input.is_empty() {
            continue;
        }
        let input = expand_command_hint(&input).to_owned();
        let _ = editor.add_history_entry(&input);
        match input.as_str() {
            "/quit" => break,
            "/new" => {
                agents.reset();
                active_session_id = None;
                println!("{}", style("Новая сессия начата.").yellow());
                continue;
            }
            "/sessions" => {
                let saved = sessions.list()?;
                if saved.is_empty() {
                    println!("{}", style("Сохранённых сессий пока нет.").dim());
                    continue;
                }
                let choices = saved
                    .iter()
                    .map(|(_, title, updated_at)| format!("{title} · {updated_at} UTC"))
                    .collect::<Vec<_>>();
                let selected = Select::with_theme(&ColorfulTheme::default())
                    .with_prompt("Выберите сессию для продолжения")
                    .items(&choices)
                    .default(0)
                    .interact_opt()?;
                let Some(selected) = selected else {
                    println!("{}", style("Выбор сессии отменён.").dim());
                    continue;
                };
                let session_id = saved[selected].0;
                let session_title = &saved[selected].1;
                let actions = ["Продолжить", "Удалить"];
                let action = Select::with_theme(&ColorfulTheme::default())
                    .with_prompt("Выберите действие")
                    .items(&actions)
                    .default(0)
                    .interact_opt()?;
                let Some(action) = action else {
                    println!("{}", style("Действие отменено.").dim());
                    continue;
                };
                if action == 1 {
                    let confirmed = Confirm::with_theme(&ColorfulTheme::default())
                        .with_prompt(format!("Удалить сессию «{session_title}»?"))
                        .default(false)
                        .interact()?;
                    if !confirmed {
                        println!("{}", style("Удаление отменено.").dim());
                        continue;
                    }
                    if sessions.delete(session_id)? {
                        if active_session_id == Some(session_id) {
                            agents.reset();
                            active_session_id = None;
                            println!(
                                "{} {}",
                                style("Сессия удалена.").yellow(),
                                style("Начата новая сессия.").dim()
                            );
                        } else {
                            println!("{}", style("Сессия удалена.").yellow());
                        }
                    } else {
                        println!("{}", style("Сессия уже была удалена.").dim());
                    }
                    continue;
                }
                let session = sessions.load(session_id)?;
                provider = session.provider;
                authorize_if_needed(&mut config, provider, &config_path)?;
                config.last_provider = Some(provider);
                config.set_model(provider, session.model.clone())?;
                config.set_temperature(provider, session.temperature)?;
                active_mode = session
                    .mode
                    .as_deref()
                    .and_then(|name| modes.modes.iter().position(|mode| mode.name == name));
                config.last_mode = active_mode.map(|index| modes.modes[index].name.clone());
                config.save(&config_path)?;
                agents.reconfigure(AgentSettings::from_config(
                    &config,
                    provider,
                    active_mode.and_then(|index| modes.modes.get(index)),
                )?);
                agents.restore(session.messages);
                active_session_id = Some(session.id);
                println!(
                    "{} {}. {} {}. {} {}.\n",
                    style("Сессия продолжена:").yellow(),
                    style(session.title).cyan().bold(),
                    style("Провайдер:").dim(),
                    style(provider).cyan(),
                    style("Модель:").dim(),
                    style(session.model).cyan()
                );
                continue;
            }
            "/provider" => {
                provider = choose_provider()?;
                authorize_if_needed(&mut config, provider, &config_path)?;
                remember_provider(&mut config, provider, &config_path)?;
                agents.reconfigure(AgentSettings::from_config(
                    &config,
                    provider,
                    active_mode.and_then(|index| modes.modes.get(index)),
                )?);
                active_session_id = None;
                println!(
                    "{} {}. {} {}. {} {}\n",
                    style("Провайдер изменён на").yellow(),
                    style(provider).cyan().bold(),
                    style("Модель:").dim(),
                    style(config.model(provider)?).cyan().bold(),
                    style("Температура:").dim(),
                    style(format_temperature(config.temperature(provider)?))
                        .cyan()
                        .bold()
                );
                continue;
            }
            "/model" => {
                let Some(model) = choose_model(&client, &config, provider).await? else {
                    println!("{}", style("Выбор модели отменён.").dim());
                    continue;
                };
                config.set_model(provider, model.clone())?;
                let temperature =
                    normalized_temperature(provider, &model, config.temperature(provider)?);
                config.set_temperature(provider, temperature)?;
                config.save(&config_path)?;
                agents.reconfigure(AgentSettings::from_config(
                    &config,
                    provider,
                    active_mode.and_then(|index| modes.modes.get(index)),
                )?);
                active_session_id = None;
                println!(
                    "{} {}. {} {}. {}\n",
                    style("Модель изменена на").yellow(),
                    style(&model).cyan().bold(),
                    style("Температура:").dim(),
                    style(format_temperature(temperature)).cyan().bold(),
                    style("Начата новая сессия.").dim()
                );
                continue;
            }
            "/mode" => {
                active_mode = choose_mode(&mut config, &mut modes, &config_path, &modes_path)?;
                agents.reconfigure(AgentSettings::from_config(
                    &config,
                    provider,
                    active_mode.and_then(|index| modes.modes.get(index)),
                )?);
                active_session_id = None;
                println!(
                    "{} {}. {}\n",
                    style("Режим изменён на").yellow(),
                    style(mode_name(&modes, active_mode)).cyan().bold(),
                    style("Начата новая сессия.").dim()
                );
                continue;
            }
            "/temperature" => {
                let Some(temperature) = choose_temperature(
                    provider,
                    config.model(provider)?,
                    config.temperature(provider)?,
                )?
                else {
                    continue;
                };
                config.set_temperature(provider, temperature)?;
                config.save(&config_path)?;
                agents.reconfigure(AgentSettings::from_config(
                    &config,
                    provider,
                    active_mode.and_then(|index| modes.modes.get(index)),
                )?);
                active_session_id = None;
                println!(
                    "{} {}. {}\n",
                    style("Температура изменена на").yellow(),
                    style(format_temperature(temperature)).cyan().bold(),
                    style("Начата новая сессия.").dim()
                );
                continue;
            }
            "/help" => {
                print_help();
                continue;
            }
            command if command.starts_with('/') => {
                println!("{}", style("Неизвестная команда. Используйте /help.").red());
                continue;
            }
            _ => {}
        }
        print!("{} ", style("● Агент 1 · выполняется…").yellow());
        std::io::stdout().flush()?;
        let results = agents.ask_all(&input).await;
        let answered = results.iter().any(|run| run.result.is_ok());
        print!("\r{}\r", " ".repeat(60));
        for run in results {
            match run.result {
                Ok(answer) => {
                    if let Some(path) = &metrics_log_path {
                        let entry = MetricsLogEntry {
                            timestamp_unix_ms: SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis(),
                            agent_id: run.agent_id,
                            provider,
                            model: config.model(provider)?,
                            elapsed_ms: run.elapsed.as_millis(),
                            request_input_tokens: answer.input_tokens,
                            request_output_tokens: answer.output_tokens,
                            session_input_tokens: answer.session_input_tokens,
                            session_output_tokens: answer.session_output_tokens,
                        };
                        if let Err(error) = append_metrics_log(path, &entry) {
                            eprintln!("{} {error:#}", style("Не удалось записать метрики:").red());
                        }
                    }
                    println!(
                        "{} {}\n{}\n",
                        style("✓").green().bold(),
                        style(format!("Агент {}", run.agent_id)).magenta().bold(),
                        answer.text
                    );
                    println!(
                        "{}\n",
                        style(format!(
                            "Метрики: {:.3} с; токены — запрос: {} входных, {} выходных; сессия: {} входных, {} выходных",
                            run.elapsed.as_secs_f64(),
                            answer.input_tokens,
                            answer.output_tokens,
                            answer.session_input_tokens,
                            answer.session_output_tokens
                        ))
                        .dim()
                    );
                }
                Err(err) => {
                    eprintln!(
                        "{} {}: {err:#}\n",
                        style("✗").red().bold(),
                        style(format!("Агент {}", run.agent_id)).red().bold()
                    );
                }
            }
        }
        if answered {
            active_session_id = Some(sessions.save(
                active_session_id,
                provider,
                config.model(provider)?,
                active_mode.and_then(|index| modes.modes.get(index).map(|mode| mode.name.as_str())),
                config.temperature(provider)?,
                agents.persisted_history(),
            )?);
        }
    }
    println!("{}", style("До встречи! 🦊").magenta());
    Ok(())
}

fn expand_command_hint(input: &str) -> &str {
    if !input.starts_with('/') || input.contains(char::is_whitespace) {
        return input;
    }
    COMMANDS
        .iter()
        .map(|(command, _)| *command)
        .find(|command| command.starts_with(input))
        .unwrap_or(input)
}

fn config_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("не удалось определить текущую директорию")?
        .join(CONFIG_FILE))
}

fn modes_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("не удалось определить текущую директорию")?
        .join(MODES_FILE))
}

fn sessions_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("не удалось определить текущую директорию")?
        .join(SESSIONS_FILE))
}

fn metrics_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("не удалось определить текущую директорию")?
        .join(METRICS_LOG_FILE))
}

fn parse_dump_metrics_flag(args: impl IntoIterator<Item = String>) -> Result<bool> {
    let mut dump_metrics = false;
    for argument in args {
        match argument.as_str() {
            "--dump-metrics" => dump_metrics = true,
            _ => bail!("неизвестный аргумент: {argument}. Доступен флаг --dump-metrics"),
        }
    }
    Ok(dump_metrics)
}

fn append_metrics_log(path: &Path, entry: &MetricsLogEntry<'_>) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("не удалось открыть {}", path.display()))?;
    serde_json::to_writer(&mut file, entry)?;
    writeln!(file)?;
    Ok(())
}

fn print_banner() {
    let term = Term::stdout();
    let _ = term.write_line(&style(FOX).color256(208).bold().to_string());
    println!("{}\n", style("FOX LLM — спроси у лисы").magenta().bold());
}

fn show_status_bar(provider: Provider, model: &str, mode: &str, temperature: f64) -> Result<()> {
    let status = format!(
        "{} {}  {} {}  {} {}  {} {}",
        style("Провайдер:").dim(),
        style(provider).cyan().bold(),
        style("Модель:").dim(),
        style(model).cyan().bold(),
        style("Режим:").dim(),
        style(mode).cyan().bold(),
        style("Температура:").dim(),
        style(format_temperature(temperature)).cyan().bold(),
    );
    // Draw the status one row below the input, then return the cursor to the
    // input row. It is erased as soon as readline finishes, so completed
    // prompts do not leave repeated status lines in terminal scrollback.
    print!("\n\x1b[2K{status}\x1b[1A\r");
    std::io::stdout().flush()?;
    Ok(())
}

fn clear_status_bar() -> Result<()> {
    // Enter leaves the cursor on the status row. Clear it before printing the
    // command result and reuse that row for normal output.
    print!("\r\x1b[2K");
    std::io::stdout().flush()?;
    Ok(())
}

fn print_help() {
    println!("\n  {}     сменить провайдера\n  {}        выбрать модель текущего провайдера\n  {}         выбрать или создать режим ответа\n  {}  изменить температуру ответов\n  {}          новая сессия\n  {}     открыть сохранённую сессию\n  {}         выйти\n  {}         эта подсказка\n",
        style("/provider").yellow(), style("/model").yellow(), style("/mode").yellow(), style("/temperature").yellow(), style("/new").yellow(), style("/sessions").yellow(), style("/quit").yellow(), style("/help").yellow());
}

fn format_temperature(temperature: f64) -> String {
    format!("{temperature:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn choose_temperature(provider: Provider, model: &str, current: f64) -> Result<Option<f64>> {
    let maximum = temperature_maximum(provider, model);
    if maximum == 1.0 && provider == Provider::Openai {
        println!(
            "{}",
            style(format!(
                "Модель {model} поддерживает только температуру по умолчанию 1."
            ))
            .yellow()
        );
        return Ok(None);
    }
    Input::with_theme(&ColorfulTheme::default())
        .with_prompt(format!(
            "Температура ответа для {provider} (от 0 до {})",
            format_temperature(maximum)
        ))
        .default(current)
        .validate_with(move |value: &f64| -> std::result::Result<(), String> {
            if value.is_finite() && (0.0..=maximum).contains(value) {
                Ok(())
            } else {
                Err(format!(
                    "температура для {provider} должна быть числом от 0 до {}",
                    format_temperature(maximum)
                ))
            }
        })
        .interact_text()
        .map(Some)
        .map_err(Into::into)
}

fn temperature_maximum(provider: Provider, model: &str) -> f64 {
    match provider {
        Provider::Claude => 1.0,
        Provider::Openai if is_original_gpt5_model(model) => 1.0,
        Provider::Openai => 2.0,
    }
}

fn is_original_gpt5_model(model: &str) -> bool {
    model == "gpt-5"
        || model.starts_with("gpt-5-")
        || model.starts_with("gpt-5-mini")
        || model.starts_with("gpt-5-nano")
}

fn supports_temperature_with_reasoning_none(model: &str) -> bool {
    model.starts_with("gpt-5.1")
        || model.starts_with("gpt-5.2")
        || model.starts_with("gpt-5.3")
        || model.starts_with("gpt-5.4")
        || model.starts_with("gpt-5.5")
        || model.starts_with("gpt-5.6")
}

fn normalized_temperature(provider: Provider, model: &str, current: f64) -> f64 {
    if provider == Provider::Openai && is_original_gpt5_model(model) {
        1.0
    } else {
        current.min(temperature_maximum(provider, model))
    }
}

async fn choose_model(
    client: &Client,
    config: &Config,
    provider: Provider,
) -> Result<Option<String>> {
    println!("{}", style("Получаю список доступных моделей…").dim());
    let models = fetch_models(client, config, provider).await?;
    if models.is_empty() {
        bail!("{provider} не вернул ни одной совместимой модели");
    }
    let current = config.model(provider)?;
    let default = models
        .iter()
        .position(|model| model == current)
        .unwrap_or(0);
    let selected = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt(format!(
            "Найдите модель {provider} (начните вводить название)"
        ))
        .items(&models)
        .default(default)
        .interact_opt()?;
    Ok(selected.map(|index| models[index].clone()))
}

async fn fetch_models(client: &Client, config: &Config, provider: Provider) -> Result<Vec<String>> {
    let response = match provider {
        Provider::Openai => client
            .get(OPENAI_MODELS_URL)
            .bearer_auth(
                config
                    .key(provider)
                    .ok_or_else(|| anyhow!("нет ключа OpenAI"))?,
            )
            .send()
            .await
            .context("не удалось получить модели OpenAI")?,
        Provider::Claude => client
            .get(CLAUDE_MODELS_URL)
            .query(&[("limit", 1000)])
            .header(
                "x-api-key",
                config
                    .key(provider)
                    .ok_or_else(|| anyhow!("нет ключа Claude"))?,
            )
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .context("не удалось получить модели Anthropic")?,
    };
    let (status, body) = read_response(response).await?;
    ensure_success(status, &body, &provider.to_string())?;
    parse_model_ids(&body, provider)
}

fn parse_model_ids(body: &Value, provider: Provider) -> Result<Vec<String>> {
    let data = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("API вернул список моделей в неизвестном формате"))?;
    let mut models: Vec<String> = data
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
        .filter(|id| model_supports_responses_api(provider, id))
        .map(str::to_owned)
        .collect();
    models.sort_unstable();
    models.dedup();
    Ok(models)
}

fn model_supports_responses_api(provider: Provider, model: &str) -> bool {
    if provider == Provider::Claude {
        return true;
    }

    let model = model.to_ascii_lowercase();
    let text_model = model.starts_with("gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.starts_with("ft:gpt-");
    let specialized_model = [
        "audio",
        "realtime",
        "transcribe",
        "tts",
        "image",
        "moderation",
        "search-preview",
        "deep-research",
    ]
    .iter()
    .any(|marker| model.contains(marker));
    // The models endpoint lists every model available to the account but does
    // not expose endpoint capabilities. Instruct models are documented as
    // legacy Completions-only and cannot be sent to /v1/responses.
    let legacy_completions_model = model.starts_with("gpt-3.5-turbo-instruct");

    text_model && !specialized_model && !legacy_completions_model
}

fn mode_name(config: &ModesConfig, active_mode: Option<usize>) -> &str {
    active_mode
        .and_then(|index| config.modes.get(index))
        .map(|mode| mode.name.as_str())
        .unwrap_or("Без ограничений")
}

fn choose_mode(
    config: &mut Config,
    modes: &mut ModesConfig,
    config_path: &Path,
    modes_path: &Path,
) -> Result<Option<usize>> {
    let mut choices = vec!["Без ограничений".to_owned()];
    choices.extend(modes.modes.iter().map(|mode| mode.name.clone()));
    choices.push("＋ Создать новый режим".to_owned());
    let selected = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Выберите режим ответа")
        .items(&choices)
        .default(0)
        .interact()?;

    if selected == 0 {
        config.last_mode = None;
        config.save(config_path)?;
        return Ok(None);
    }
    if selected <= modes.modes.len() {
        let index = selected - 1;
        config.last_mode = Some(modes.modes[index].name.clone());
        config.save(config_path)?;
        return Ok(Some(index));
    }

    let mode = create_mode()?;
    let name = mode.name.clone();
    let index = if let Some(index) = modes
        .modes
        .iter()
        .position(|existing| existing.name == name)
    {
        let overwrite = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(format!("Режим «{name}» уже существует. Заменить его?"))
            .default(false)
            .interact()?;
        if !overwrite {
            bail!("создание режима отменено");
        }
        modes.modes[index] = mode;
        index
    } else {
        modes.modes.push(mode);
        modes.modes.len() - 1
    };
    config.last_mode = Some(name);
    modes.save(modes_path)?;
    config.save(config_path)?;
    Ok(Some(index))
}

fn create_mode() -> Result<ResponseMode> {
    let name: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Название режима")
        .validate_with(|value: &String| -> std::result::Result<(), &str> {
            if value.trim().is_empty() {
                Err("название не может быть пустым")
            } else {
                Ok(())
            }
        })
        .interact_text()?;
    let name = name.trim().to_owned();
    if name == "Без ограничений" {
        bail!("режим с названием «{name}» уже существует");
    }

    let instructions = prompt_multiline("Инструкции (формат, стиль и другие требования)")?;
    Ok(ResponseMode { name, instructions })
}

fn prompt_multiline(prompt: &str) -> Result<String> {
    println!("{prompt}");
    println!(
        "{}",
        style("Вставьте многострочный текст и завершите отдельной строкой /done.").dim()
    );
    let stdin = std::io::stdin();
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            bail!("ввод инструкций прерван");
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line == "/done" {
            break;
        }
        lines.push(line.to_owned());
    }
    Ok(lines.join("\n").trim().to_owned())
}

fn choose_provider() -> Result<Provider> {
    let choices = Provider::all();
    let names: Vec<String> = choices.iter().map(ToString::to_string).collect();
    let selected = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Выберите API-провайдера")
        .items(&names)
        .default(0)
        .interact()?;
    Ok(choices[selected])
}

fn authorize_if_needed(config: &mut Config, provider: Provider, path: &Path) -> Result<()> {
    if config.key(provider).is_some() {
        return Ok(());
    }
    println!("\nДля {} нужен API-ключ.", style(provider).cyan().bold());
    let method = choose_auth_method()?;
    let prompt = match method {
        AuthMethod::CreateInWeb => {
            println!(
                "Открываю официальную страницу: {}",
                style(provider.key_url()).underlined()
            );
            if webbrowser::open(provider.key_url()).is_err() {
                println!("Не удалось открыть браузер — перейдите по ссылке вручную.");
            }
            "Вставьте созданный ключ (ввод скрыт): "
        }
        AuthMethod::ExistingKey => "Введите существующий ключ (ввод скрыт): ",
    };
    let key = prompt_masked_key(prompt)?;
    let key = key.trim().to_owned();
    if key.is_empty() {
        bail!("API-ключ не может быть пустым");
    }
    config.set_key(provider, key);
    config.save(path)?;
    println!(
        "{}\n",
        style(format!("Ключ сохранён в {}", path.display())).green()
    );
    Ok(())
}

fn choose_auth_method() -> Result<AuthMethod> {
    let methods = ["Создать новый ключ в вебе", "Ввести существующий ключ"];
    let selected = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Как добавить API-ключ?")
        .items(&methods)
        .default(0)
        .interact()?;
    Ok(match selected {
        0 => AuthMethod::CreateInWeb,
        _ => AuthMethod::ExistingKey,
    })
}

fn prompt_masked_key(prompt: &str) -> Result<String> {
    let term = Term::stderr();
    term.write_str(prompt)?;
    term.flush()?;
    let mut key = String::new();
    loop {
        match term.read_key()? {
            Key::Char(character) if !character.is_control() => {
                key.push(character);
                term.write_str("*")?;
                term.flush()?;
            }
            Key::Backspace if key.pop().is_some() => {
                term.clear_chars(1)?;
                term.flush()?;
            }
            Key::Enter => {
                term.write_line("")?;
                return Ok(key);
            }
            Key::CtrlC | Key::Escape => {
                term.write_line("")?;
                bail!("ввод API-ключа отменён");
            }
            _ => {}
        }
    }
}

fn remember_provider(config: &mut Config, provider: Provider, path: &Path) -> Result<()> {
    config.last_provider = Some(provider);
    config.save(path)
}

async fn send_request(
    client: &Client,
    settings: &AgentSettings,
    history: &[Message],
) -> Result<ApiAnswer> {
    match settings.provider {
        Provider::Openai => send_openai(client, settings, history).await,
        Provider::Claude => send_claude(client, settings, history).await,
    }
}

async fn send_openai(
    client: &Client,
    settings: &AgentSettings,
    history: &[Message],
) -> Result<ApiAnswer> {
    let mut payload = json!({
        "model": settings.model,
        "input": history,
        "temperature": settings.temperature
    });
    if supports_temperature_with_reasoning_none(&settings.model) {
        payload["reasoning"] = json!({ "effort": "none" });
    }
    if let Some(instructions) = &settings.instructions {
        payload["instructions"] = json!(instructions);
    }
    let response = client
        .post("https://api.openai.com/v1/responses")
        .bearer_auth(&settings.api_key)
        .json(&payload)
        .send()
        .await
        .context("не удалось подключиться к OpenAI")?;
    let (status, body) = read_response(response).await?;
    ensure_success(status, &body, "OpenAI")?;
    let text = extract_openai_text(&body)?;
    let input_tokens = body
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = body
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(ApiAnswer {
        text,
        input_tokens,
        output_tokens,
        session_input_tokens: 0,
        session_output_tokens: 0,
    })
}

async fn send_claude(
    client: &Client,
    settings: &AgentSettings,
    history: &[Message],
) -> Result<ApiAnswer> {
    let mut payload = json!({
        "model": settings.model,
        "max_tokens": 4096,
        "temperature": settings.temperature,
        "messages": history
    });
    if let Some(instructions) = &settings.instructions {
        payload["system"] = json!(instructions);
    }
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &settings.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&payload)
        .send()
        .await
        .context("не удалось подключиться к Anthropic")?;
    let (status, body) = read_response(response).await?;
    ensure_success(status, &body, "Anthropic")?;
    let text = extract_claude_text(&body)?;
    let input_tokens = body
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = body
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(ApiAnswer {
        text,
        input_tokens,
        output_tokens,
        session_input_tokens: 0,
        session_output_tokens: 0,
    })
}

async fn read_response(response: reqwest::Response) -> Result<(StatusCode, Value)> {
    let status = response.status();
    let text = response
        .text()
        .await
        .context("не удалось прочитать ответ API")?;
    let body = serde_json::from_str(&text)
        .with_context(|| format!("API вернул не JSON: {}", truncate(&text, 300)))?;
    Ok((status, body))
}

fn ensure_success(status: StatusCode, body: &Value, provider: &str) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("неизвестная ошибка API");
    bail!("{provider} вернул {status}: {message}")
}

fn extract_openai_text(body: &Value) -> Result<String> {
    let parts = body
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|part| part.get("text").and_then(Value::as_str));
    nonempty_text(parts, "OpenAI не вернул текст")
}

fn extract_claude_text(body: &Value) -> Result<String> {
    let parts = body
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str));
    nonempty_text(parts, "Claude не вернул текст")
}

fn nonempty_text<'a>(parts: impl Iterator<Item = &'a str>, error: &str) -> Result<String> {
    let text = parts.collect::<Vec<_>>().join("\n");
    if text.trim().is_empty() {
        bail!(error.to_owned());
    }
    Ok(text)
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let result: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_response() {
        let body = json!({"output": [{"content": [{"type": "output_text", "text": "Привет!"}]}]});
        assert_eq!(extract_openai_text(&body).unwrap(), "Привет!");
    }

    #[test]
    fn parses_claude_response() {
        let body = json!({"content": [{"type": "text", "text": "Привет!"}]});
        assert_eq!(extract_claude_text(&body).unwrap(), "Привет!");
    }

    #[test]
    fn parses_dump_metrics_flag() {
        assert!(!parse_dump_metrics_flag(Vec::new()).unwrap());
        assert!(parse_dump_metrics_flag(vec!["--dump-metrics".into()]).unwrap());
        assert!(parse_dump_metrics_flag(vec!["--unknown".into()]).is_err());
    }

    #[test]
    fn appends_json_metrics_log() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metrics.log");
        let entry = MetricsLogEntry {
            timestamp_unix_ms: 123,
            agent_id: 1,
            provider: Provider::Openai,
            model: "gpt-test",
            elapsed_ms: 456,
            request_input_tokens: 10,
            request_output_tokens: 20,
            session_input_tokens: 30,
            session_output_tokens: 40,
        };

        append_metrics_log(&path, &entry).unwrap();
        append_metrics_log(&path, &entry).unwrap();

        let lines = fs::read_to_string(path).unwrap();
        assert_eq!(lines.lines().count(), 2);
        let value: Value = serde_json::from_str(lines.lines().next().unwrap()).unwrap();
        assert_eq!(value["request_input_tokens"], 10);
        assert_eq!(value["session_output_tokens"], 40);
    }

    #[test]
    fn config_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = Config {
            last_provider: Some(Provider::Claude),
            ..Config::default()
        };
        config.set_key(Provider::Claude, "secret".into());
        config
            .set_model(Provider::Claude, "claude-test".into())
            .unwrap();
        config.last_mode = Some("Кратко".into());
        config.set_temperature(Provider::Claude, 0.7).unwrap();
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.last_provider, Some(Provider::Claude));
        assert_eq!(loaded.key(Provider::Claude), Some("secret"));
        assert_eq!(loaded.model(Provider::Claude).unwrap(), "claude-test");
        assert_eq!(loaded.providers.len(), 2);
        assert_eq!(loaded.last_mode.as_deref(), Some("Кратко"));
        assert_eq!(loaded.temperature(Provider::Claude).unwrap(), 0.7);
        assert_eq!(loaded.temperature(Provider::Openai).unwrap(), 1.0);
    }

    #[test]
    fn modes_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("modes.json");
        let modes = ModesConfig {
            modes: vec![ResponseMode {
                name: "Кратко".into(),
                instructions: "Ответь одним предложением".into(),
            }],
        };
        modes.save(&path).unwrap();
        let loaded = ModesConfig::load(&path).unwrap();
        assert_eq!(loaded.modes.len(), 1);
        assert_eq!(loaded.modes[0].name, "Кратко");
        assert_eq!(loaded.modes[0].instructions, "Ответь одним предложением");
    }

    #[test]
    fn migrates_legacy_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
                "last_provider": "openai",
                "openai_api_key": "old-secret",
                "claude_api_key": null,
                "openai_model": "gpt-test",
                "claude_model": "claude-test"
            }"#,
        )
        .unwrap();

        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.key(Provider::Openai), Some("old-secret"));
        assert_eq!(loaded.model(Provider::Openai).unwrap(), "gpt-test");
        assert_eq!(
            loaded.temperature(Provider::Openai).unwrap(),
            default_temperature()
        );
        let migrated: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert!(migrated.get("providers").unwrap().is_array());
        assert!(migrated.get("openai_api_key").is_none());
    }

    #[test]
    fn loads_current_config_without_temperature() {
        let config: Config = serde_json::from_value(json!({
            "last_provider": "openai",
            "last_mode": null,
            "providers": [{
                "provider": "openai",
                "api_key": null,
                "model": "gpt-4.1-mini"
            }]
        }))
        .unwrap();

        assert_eq!(
            config.temperature(Provider::Openai).unwrap(),
            default_temperature()
        );
        assert_eq!(format_temperature(0.0), "0");
        assert_eq!(format_temperature(0.7), "0.7");
        assert_eq!(format_temperature(1.0), "1");
        assert_eq!(temperature_maximum(Provider::Openai, "gpt-4o"), 2.0);
        assert_eq!(temperature_maximum(Provider::Openai, "gpt-5-mini"), 1.0);
        assert!(supports_temperature_with_reasoning_none("gpt-5.6-luna"));
        assert!(!supports_temperature_with_reasoning_none("gpt-5-mini"));
        assert_eq!(
            temperature_maximum(Provider::Claude, "claude-sonnet-5"),
            1.0
        );
    }

    #[test]
    fn completes_slash_commands() {
        let history = DefaultHistory::new();
        let context = ReadlineContext::new(&history);
        let (start, candidates) = CommandHelper.complete("/pro", 4, &context).unwrap();
        assert_eq!(start, 0);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "/provider");
        assert_eq!(
            CommandHelper.hint("/pro", 4, &context).as_deref(),
            Some("vider")
        );
        assert_eq!(expand_command_hint("/pro"), "/provider");
        assert_eq!(expand_command_hint("обычный запрос"), "обычный запрос");
        assert_eq!(expand_command_hint("/unknown"), "/unknown");
        assert_eq!(expand_command_hint("/model"), "/model");
        assert_eq!(
            CommandHelper.highlight_hint("vider").as_ref(),
            "\x1b[2mvider\x1b[0m"
        );
    }

    #[test]
    fn parses_sorts_and_deduplicates_model_ids() {
        let body = json!({
            "data": [
                {"id": "model-z"},
                {"id": "model-a", "display_name": "Model A"},
                {"id": "model-z"},
                {"display_name": "No id"},
                {"id": ""}
            ]
        });
        assert_eq!(
            parse_model_ids(&body, Provider::Claude).unwrap(),
            vec!["model-a".to_owned(), "model-z".to_owned()]
        );
        assert!(parse_model_ids(&json!({"models": []}), Provider::Openai).is_err());
    }

    #[test]
    fn filters_out_openai_models_for_other_apis() {
        let body = json!({
            "data": [
                {"id": "gpt-5.6-luna"},
                {"id": "o4-mini"},
                {"id": "ft:gpt-4.1:team:custom:id"},
                {"id": "gpt-3.5-turbo-instruct"},
                {"id": "gpt-3.5-turbo-instruct-0914"},
                {"id": "gpt-realtime"},
                {"id": "gpt-4o-mini-transcribe"},
                {"id": "gpt-image-1"},
                {"id": "o3-deep-research"},
                {"id": "text-embedding-3-small"},
                {"id": "omni-moderation-latest"},
                {"id": "davinci-002"}
            ]
        });
        assert_eq!(
            parse_model_ids(&body, Provider::Openai).unwrap(),
            vec![
                "ft:gpt-4.1:team:custom:id".to_owned(),
                "gpt-5.6-luna".to_owned(),
                "o4-mini".to_owned()
            ]
        );
    }

    #[test]
    fn normalizes_temperature_for_selected_model() {
        assert_eq!(normalized_temperature(Provider::Claude, "any", 1.7), 1.0);
        assert_eq!(
            normalized_temperature(Provider::Openai, "gpt-5-mini", 0.4),
            1.0
        );
        assert_eq!(
            normalized_temperature(Provider::Openai, "gpt-4.1", 1.7),
            1.7
        );
    }

    fn test_agent_settings() -> AgentSettings {
        AgentSettings {
            provider: Provider::Openai,
            api_key: "test-key".into(),
            model: "test-model".into(),
            temperature: 0.5,
            instructions: Some("Отвечай кратко".into()),
        }
    }

    #[test]
    fn creates_independent_agents_in_pool() {
        let client = Client::new();
        let pool = AgentPool::new(3, client, test_agent_settings());

        assert_eq!(pool.agents.len(), 3);
        assert_eq!(
            pool.agents.iter().map(|agent| agent.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(pool.agents.iter().all(|agent| agent.history.is_empty()));
        assert!(pool
            .agents
            .iter()
            .all(|agent| agent.status == AgentStatus::Idle));
    }

    #[test]
    fn reconfiguring_pool_resets_every_agent() {
        let client = Client::new();
        let mut pool = AgentPool::new(2, client, test_agent_settings());
        for agent in &mut pool.agents {
            agent.history.push(Message {
                role: "user".to_owned(),
                content: "старый запрос".into(),
            });
            agent.status = AgentStatus::Completed;
        }

        let mut settings = test_agent_settings();
        settings.temperature = 0.9;
        settings.instructions = Some("Новый режим".into());
        pool.reconfigure(settings);

        assert!(pool.agents.iter().all(|agent| agent.history.is_empty()));
        assert!(pool
            .agents
            .iter()
            .all(|agent| agent.status == AgentStatus::Idle));
        assert!(pool
            .agents
            .iter()
            .all(|agent| agent.settings.temperature == 0.9));
        assert!(pool
            .agents
            .iter()
            .all(|agent| agent.settings.instructions.as_deref() == Some("Новый режим")));
    }

    #[test]
    fn toon_history_round_trips_multiline_messages() {
        let messages = vec![
            Message {
                role: "user".into(),
                content: "Привет, лиса!\nКак дела?".into(),
            },
            Message {
                role: "assistant".into(),
                content: "Хорошо: \"отлично\"".into(),
            },
        ];
        let encoded = encode_messages_toon(&messages);
        assert!(encoded.starts_with("messages[2]{role,content}:"));
        assert_eq!(decode_messages_toon(&encoded).unwrap(), messages);
    }

    #[test]
    fn sqlite_store_saves_and_loads_session() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::open(&directory.path().join("sessions.db")).unwrap();
        let messages = vec![Message {
            role: "user".into(),
            content: "Обсудим сохранение контекста".into(),
        }];
        let id = store
            .save(
                None,
                Provider::Openai,
                "test-model",
                Some("Кратко"),
                0.5,
                &messages,
            )
            .unwrap();
        let loaded = store.load(id).unwrap();
        assert_eq!(loaded.messages, messages);
        assert_eq!(loaded.model, "test-model");
        assert_eq!(loaded.mode.as_deref(), Some("Кратко"));
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(store.delete(id).unwrap());
        assert!(store.list().unwrap().is_empty());
        assert!(!store.delete(id).unwrap());
    }
}
