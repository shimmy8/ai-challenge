use anyhow::{anyhow, bail, Context, Result};
use console::{style, Key, Term};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use reqwest::{Client, StatusCode};
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
};

const CONFIG_FILE: &str = ".fox-llm.json";
const OPENAI_KEYS_URL: &str = "https://platform.openai.com/api-keys";
const CLAUDE_KEYS_URL: &str = "https://console.anthropic.com/settings/keys";
const COMMANDS: &[(&str, &str)] = &[
    ("/provider", "сменить провайдера"),
    ("/mode", "выбрать или создать режим ответа"),
    ("/new", "начать новую сессию"),
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
    #[serde(default)]
    modes: Vec<ResponseMode>,
    providers: Vec<ProviderConfig>,
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
    "gpt-5-mini".into()
}
fn default_claude_model() -> String {
    "claude-sonnet-5".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            last_provider: None,
            last_mode: None,
            modes: Vec::new(),
            providers: vec![
                ProviderConfig {
                    provider: Provider::Openai,
                    api_key: None,
                    model: default_openai_model(),
                },
                ProviderConfig {
                    provider: Provider::Claude,
                    api_key: None,
                    model: default_claude_model(),
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
            modes: Vec::new(),
            providers: vec![
                ProviderConfig {
                    provider: Provider::Openai,
                    api_key: legacy.openai_api_key,
                    model: legacy.openai_model,
                },
                ProviderConfig {
                    provider: Provider::Claude,
                    api_key: legacy.claude_api_key,
                    model: legacy.claude_model,
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

    fn provider(&self, provider: Provider) -> Option<&ProviderConfig> {
        self.providers.iter().find(|item| item.provider == provider)
    }
}

#[derive(Debug, Clone, Serialize)]
struct Message {
    role: &'static str,
    content: String,
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
    print_banner();
    let config_path = config_path()?;
    let mut config = Config::load(&config_path)?;
    let mut provider = match config.last_provider {
        Some(saved) => saved,
        None => choose_provider()?,
    };
    authorize_if_needed(&mut config, provider, &config_path)?;
    remember_provider(&mut config, provider, &config_path)?;
    let mut active_mode = config
        .last_mode
        .as_deref()
        .and_then(|name| config.modes.iter().position(|mode| mode.name == name));
    println!(
        "{} {}. {} {}. Введите {} для списка команд.\n",
        style("Провайдер:").dim(),
        style(provider).cyan().bold(),
        style("Режим:").dim(),
        style(mode_name(&config, active_mode)).cyan().bold(),
        style("/help").yellow()
    );

    let client = Client::builder().user_agent("fox-llm/0.1.0").build()?;
    let mut history: Vec<Message> = Vec::new();
    let mut editor = Editor::<CommandHelper, DefaultHistory>::new()?;
    editor.set_helper(Some(CommandHelper));
    loop {
        let prompt = format!("{} ", style("Вы ›").green().bold());
        let input = match editor.readline(&prompt) {
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
                history.clear();
                println!("{}", style("Новая сессия начата.").yellow());
                continue;
            }
            "/provider" => {
                provider = choose_provider()?;
                authorize_if_needed(&mut config, provider, &config_path)?;
                remember_provider(&mut config, provider, &config_path)?;
                history.clear();
                println!(
                    "{} {}\n",
                    style("Провайдер изменён на").yellow(),
                    style(provider).cyan().bold()
                );
                continue;
            }
            "/mode" => {
                active_mode = choose_mode(&mut config, &config_path)?;
                history.clear();
                println!(
                    "{} {}. {}\n",
                    style("Режим изменён на").yellow(),
                    style(mode_name(&config, active_mode)).cyan().bold(),
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
        history.push(Message {
            role: "user",
            content: input,
        });
        print!("{} ", style("Лиса думает…").dim());
        std::io::stdout().flush()?;
        let mode = active_mode.and_then(|index| config.modes.get(index));
        let result = send_request(&client, &config, provider, &history, mode).await;
        print!("\r{}\r", " ".repeat(40));
        match result {
            Ok(answer) => {
                println!("{}\n{}\n", style("Лиса").magenta().bold(), answer);
                history.push(Message {
                    role: "assistant",
                    content: answer,
                });
            }
            Err(err) => {
                history.pop();
                eprintln!("{} {err:#}\n", style("Ошибка:").red().bold());
            }
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

fn print_banner() {
    let term = Term::stdout();
    let _ = term.write_line(&style(FOX).color256(208).bold().to_string());
    println!("{}\n", style("FOX LLM — спроси у лисы").magenta().bold());
}

fn print_help() {
    println!("\n  {}  сменить провайдера\n  {}      выбрать или создать режим ответа\n  {}       новая сессия\n  {}      выйти\n  {}      эта подсказка\n",
        style("/provider").yellow(), style("/mode").yellow(), style("/new").yellow(), style("/quit").yellow(), style("/help").yellow());
}

fn mode_name(config: &Config, active_mode: Option<usize>) -> &str {
    active_mode
        .and_then(|index| config.modes.get(index))
        .map(|mode| mode.name.as_str())
        .unwrap_or("Без ограничений")
}

fn choose_mode(config: &mut Config, path: &Path) -> Result<Option<usize>> {
    let mut choices = vec!["Без ограничений".to_owned()];
    choices.extend(config.modes.iter().map(|mode| mode.name.clone()));
    choices.push("＋ Создать новый режим".to_owned());
    let selected = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Выберите режим ответа")
        .items(&choices)
        .default(0)
        .interact()?;

    if selected == 0 {
        config.last_mode = None;
        config.save(path)?;
        return Ok(None);
    }
    if selected <= config.modes.len() {
        let index = selected - 1;
        config.last_mode = Some(config.modes[index].name.clone());
        config.save(path)?;
        return Ok(Some(index));
    }

    let mode = create_mode()?;
    let name = mode.name.clone();
    let index = if let Some(index) = config
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
        config.modes[index] = mode;
        index
    } else {
        config.modes.push(mode);
        config.modes.len() - 1
    };
    config.last_mode = Some(name);
    config.save(path)?;
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
    config: &Config,
    provider: Provider,
    history: &[Message],
    mode: Option<&ResponseMode>,
) -> Result<String> {
    match provider {
        Provider::Openai => send_openai(client, config, history, mode).await,
        Provider::Claude => send_claude(client, config, history, mode).await,
    }
}

async fn send_openai(
    client: &Client,
    config: &Config,
    history: &[Message],
    mode: Option<&ResponseMode>,
) -> Result<String> {
    let mut payload = json!({ "model": config.model(Provider::Openai)?, "input": history });
    if let Some(mode) = mode {
        if !mode.instructions.is_empty() {
            payload["instructions"] = json!(mode.instructions);
        }
    }
    let response = client
        .post("https://api.openai.com/v1/responses")
        .bearer_auth(
            config
                .key(Provider::Openai)
                .ok_or_else(|| anyhow!("нет ключа OpenAI"))?,
        )
        .json(&payload)
        .send()
        .await
        .context("не удалось подключиться к OpenAI")?;
    let (status, body) = read_response(response).await?;
    ensure_success(status, &body, "OpenAI")?;
    extract_openai_text(&body)
}

async fn send_claude(
    client: &Client,
    config: &Config,
    history: &[Message],
    mode: Option<&ResponseMode>,
) -> Result<String> {
    let mut payload = json!({
        "model": config.model(Provider::Claude)?,
        "max_tokens": 4096,
        "messages": history
    });
    if let Some(mode) = mode.filter(|mode| !mode.instructions.is_empty()) {
        payload["system"] = json!(mode.instructions);
    }
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header(
            "x-api-key",
            config
                .key(Provider::Claude)
                .ok_or_else(|| anyhow!("нет ключа Claude"))?,
        )
        .header("anthropic-version", "2023-06-01")
        .json(&payload)
        .send()
        .await
        .context("не удалось подключиться к Anthropic")?;
    let (status, body) = read_response(response).await?;
    ensure_success(status, &body, "Anthropic")?;
    extract_claude_text(&body)
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
    fn config_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = Config {
            last_provider: Some(Provider::Claude),
            ..Config::default()
        };
        config.set_key(Provider::Claude, "secret".into());
        config.modes.push(ResponseMode {
            name: "Кратко".into(),
            instructions: "Ответь одним предложением".into(),
        });
        config.last_mode = Some("Кратко".into());
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.last_provider, Some(Provider::Claude));
        assert_eq!(loaded.key(Provider::Claude), Some("secret"));
        assert_eq!(loaded.providers.len(), 2);
        assert_eq!(loaded.last_mode.as_deref(), Some("Кратко"));
        assert_eq!(loaded.modes.len(), 1);
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
        let migrated: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert!(migrated.get("providers").unwrap().is_array());
        assert!(migrated.get("openai_api_key").is_none());
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
        assert_eq!(
            CommandHelper.highlight_hint("vider").as_ref(),
            "\x1b[2mvider\x1b[0m"
        );
    }
}
