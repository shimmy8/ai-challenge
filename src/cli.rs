#![allow(unused_imports)]
use crate::{agent::*, config::*, model::*, providers::*, sessions::*};
use anyhow::{anyhow, bail, Context, Result};
use console::{style, Key, Term};
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, Select};
use reqwest::{Client, StatusCode};
use rusqlite::{params, Connection, OptionalExtension};
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
pub(crate) struct CommandHelper {
    pub(crate) branching_enabled: Arc<AtomicBool>,
}

impl CommandHelper {
    pub(crate) fn new(branching_enabled: Arc<AtomicBool>) -> Self {
        Self { branching_enabled }
    }

    pub(crate) fn commands(&self) -> Vec<(&'static str, &'static str)> {
        available_commands(self.branching_enabled.load(Ordering::Relaxed))
    }
}

pub(crate) fn available_commands(branching_enabled: bool) -> Vec<(&'static str, &'static str)> {
    let mut commands = COMMANDS.to_vec();
    if branching_enabled {
        commands.extend_from_slice(BRANCHING_COMMANDS);
    }
    commands
}

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
        let candidates = self
            .commands()
            .into_iter()
            .filter(|(command, _)| command.starts_with(prefix))
            .map(|(command, description)| Pair {
                display: format!("{command:<12} {description}"),
                replacement: command.to_owned(),
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
        let commands = self.commands();
        if commands.iter().any(|(command, _)| *command == line) {
            return None;
        }
        let matches = commands
            .into_iter()
            .map(|(command, _)| command)
            .filter(|command| command.starts_with(line))
            .collect::<Vec<_>>();
        (matches.len() == 1).then(|| matches[0][line.len()..].to_owned())
    }
}
pub(crate) fn expand_command_hint(input: &str, branching_enabled: bool) -> &str {
    if !input.starts_with('/') || input.contains(char::is_whitespace) {
        return input;
    }
    let commands = available_commands(branching_enabled);
    if commands.iter().any(|(command, _)| *command == input) {
        return input;
    }
    let matches = commands
        .into_iter()
        .map(|(command, _)| command)
        .filter(|command| command.starts_with(input))
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        matches[0]
    } else {
        input
    }
}

pub(crate) fn config_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("не удалось определить текущую директорию")?
        .join(CONFIG_FILE))
}

pub(crate) fn modes_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("не удалось определить текущую директорию")?
        .join(MODES_FILE))
}

pub(crate) fn sessions_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("не удалось определить текущую директорию")?
        .join(SESSIONS_FILE))
}

pub(crate) fn print_banner() {
    let term = Term::stdout();
    let _ = term.write_line(&style(FOX).color256(208).bold().to_string());
    println!("{}\n", style("FOX LLM — спроси у лисы").magenta().bold());
}

pub(crate) struct StatusBar<'a> {
    pub(crate) provider: Provider,
    pub(crate) model: &'a str,
    pub(crate) mode: &'a str,
    pub(crate) temperature: f64,
    pub(crate) strategy: CompressionStrategy,
    pub(crate) context_messages: usize,
    pub(crate) active_branch: Option<&'a str>,
    pub(crate) memory: &'a ActiveMemory,
}

pub(crate) fn show_status_bar(status_bar: StatusBar<'_>) -> Result<()> {
    let compression = if status_bar.strategy == CompressionStrategy::Branching {
        format!(
            "{}:{}",
            status_bar.strategy,
            status_bar.active_branch.unwrap_or("main")
        )
    } else {
        format!("{}:{}", status_bar.strategy, status_bar.context_messages)
    };
    let profile = status_bar
        .memory
        .profile
        .as_ref()
        .map(|value| value.name.as_str());
    let task = status_bar
        .memory
        .task
        .as_ref()
        .map(|value| value.title.as_str());
    let status = format!(
        "{} {}  {} {}  {} {}  {} {}  {} {}{}{}",
        style("Сжатие:").dim(),
        style(compression).cyan().bold(),
        style("Провайдер:").dim(),
        style(status_bar.provider).cyan().bold(),
        style("Модель:").dim(),
        style(status_bar.model).cyan().bold(),
        style("Режим:").dim(),
        style(status_bar.mode).cyan().bold(),
        style("Температура:").dim(),
        style(format_temperature(status_bar.temperature))
            .cyan()
            .bold(),
        profile
            .map(|_| format!("  {} ", style("Профиль:").dim()))
            .unwrap_or_default(),
        profile
            .map(|name| style(name).cyan().bold().to_string())
            .unwrap_or_default(),
    );
    // Draw the status one row below the input, then return the cursor to the
    // input row. It is erased as soon as readline finishes, so completed
    // prompts do not leave repeated status lines in terminal scrollback.
    let width = usize::from(Term::stdout().size().1).saturating_sub(1);
    let status = console::truncate_str(&status, width, "…");
    if let (Some(task), Some(phase)) = (task, active_task_phase_line(status_bar.memory, false)) {
        let status = format!(
            "{status}  {} {}",
            style("Задача:").dim(),
            style(task).cyan().bold()
        );
        let status = console::truncate_str(&status, width, "…");
        let phase = console::truncate_str(&phase, width, "…");
        print!("{}", status_render_sequence(Some(&phase), &status));
    } else {
        print!("{}", status_render_sequence(None, &status));
    }
    std::io::stdout().flush()?;
    Ok(())
}

pub(crate) fn status_render_sequence(phase: Option<&str>, status: &str) -> String {
    phase.map_or_else(
        || format!("\n\x1b[2K{status}\x1b[1A\r"),
        |phase| format!("\n\x1b[2K{phase}\n\x1b[2K{status}\x1b[2A\r"),
    )
}

pub(crate) fn clear_status_bar(has_task: bool) -> Result<()> {
    // Enter leaves the cursor on the status row. Clear it before printing the
    // command result and reuse that row for normal output.
    print!("{}", status_clear_sequence(has_task));
    std::io::stdout().flush()?;
    Ok(())
}

pub(crate) fn status_clear_sequence(has_task: bool) -> &'static str {
    if has_task {
        "\r\x1b[2K\x1b[1A\r\x1b[2K"
    } else {
        "\r\x1b[2K"
    }
}

pub(crate) fn active_task_phase_line(memory: &ActiveMemory, force_color: bool) -> Option<String> {
    memory
        .task
        .as_ref()
        .map(|task| format_task_phase_line(task.phase, force_color))
}

pub(crate) fn format_task_phase_line(phase: TaskPhase, force_color: bool) -> String {
    let phase = match phase {
        TaskPhase::Planning => style(phase).blue().bold(),
        TaskPhase::Execution => style(phase).yellow().bold(),
        TaskPhase::Validation => style(phase).magenta().bold(),
        TaskPhase::Done => style(phase).green().bold(),
    };
    let phase = if force_color {
        phase.force_styling(true)
    } else {
        phase
    };
    format!("{} {phase}", style("Этап задачи:").dim())
}

pub(crate) fn format_memory(memory: &ActiveMemory, message_count: usize) -> String {
    let profile = memory.profile.as_ref().map_or_else(
        || "не выбран".to_owned(),
        |profile| {
            format!(
                "#{} «{}»\n{}",
                profile.id, profile.name, profile.instructions
            )
        },
    );
    let task = memory.task.as_ref().map_or_else(
        || "не выбрана".to_owned(),
        |task| {
            format!(
                "#{} «{}»\nФаза: {}\nTODO: {}",
                task.id, task.title, task.phase, task.todo
            )
        },
    );
    format!(
        "Краткосрочная память (сессия): {message_count} сообщений\n\nРабочая память (задача):\n{task}\n\nДолговременная память (профиль):\n{profile}"
    )
}

pub(crate) fn print_help(branching_enabled: bool) {
    println!();
    for (command, description) in available_commands(branching_enabled) {
        println!("  {:<14} {}", style(command).yellow(), description);
    }
    println!();
}

pub(crate) fn parse_context_messages(value: &str) -> Result<usize> {
    let size: usize = value.parse().context("ожидалось целое число")?;
    anyhow::ensure!(size <= 1000, "размер вне диапазона");
    Ok(size)
}

pub(crate) fn parse_compression_strategy(value: &str) -> Result<CompressionStrategy> {
    match value {
        "summary" => Ok(CompressionStrategy::Summary),
        "sliding" | "sliding-window" => Ok(CompressionStrategy::SlidingWindow),
        "facts" | "sticky-facts" => Ok(CompressionStrategy::StickyFacts),
        "branching" => Ok(CompressionStrategy::Branching),
        _ => bail!("неизвестная стратегия: {value}"),
    }
}

pub(crate) fn handle_branching_command(command: &str, agents: &mut AgentPool) -> Result<()> {
    match command.split_whitespace().next().unwrap_or_default() {
        "/checkpoint" => {
            for agent in &mut agents.agents {
                agent.create_checkpoint();
            }
            println!(
                "{}",
                style("Checkpoint сохранён. Следующее сообщение создаст новую ветку.").yellow()
            );
        }
        "/load" => {
            for agent in &mut agents.agents {
                agent.load_checkpoint()?;
            }
            println!(
                "{}",
                style("Checkpoint загружен. Следующее сообщение создаст новую ветку.").yellow()
            );
            match agents
                .agents
                .first()
                .and_then(Agent::last_assistant_message)
            {
                Some(message) => println!(
                    "{}\n{}\n",
                    style("Последний ответ модели перед checkpoint:").dim(),
                    message
                ),
                None => println!(
                    "{}\n",
                    style("До checkpoint ещё не было ответов модели.").dim()
                ),
            }
        }
        "/switch" => {
            let argument = command.strip_prefix("/switch").unwrap_or_default().trim();
            let name = if argument.is_empty() {
                let Some(agent) = agents.agents.first() else {
                    return Ok(());
                };
                let mut names = agent.branches.keys().cloned().collect::<Vec<_>>();
                names.sort();
                if names.is_empty() {
                    println!(
                        "{}",
                        style("Веток пока нет. Сначала создайте /checkpoint.").yellow()
                    );
                    return Ok(());
                }
                let default = names
                    .iter()
                    .position(|name| name == &agent.active_branch)
                    .unwrap_or(0);
                let selected = Select::with_theme(&ColorfulTheme::default())
                    .with_prompt("Выберите ветку")
                    .items(&names)
                    .default(default)
                    .interact_opt()?;
                let Some(selected) = selected else {
                    println!("{}", style("Переключение отменено.").dim());
                    return Ok(());
                };
                names[selected].clone()
            } else {
                argument.to_owned()
            };
            for agent in &mut agents.agents {
                agent.switch_branch(&name)?;
            }
            println!("Активна ветка «{name}».");
        }
        "/branches" => {
            if let Some(agent) = agents.agents.first() {
                let mut names = agent.branches.keys().collect::<Vec<_>>();
                names.sort();
                for name in names {
                    let marker = if name == &agent.active_branch {
                        "*"
                    } else {
                        " "
                    };
                    println!("  {marker} {name}");
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

pub(crate) fn handle_compression_command(
    arguments: &str,
    config: &mut Config,
    config_path: &Path,
    agents: &mut AgentPool,
) -> Result<()> {
    let mut parts = arguments.split_whitespace();
    let action = parts.next();
    let strategy = if let Some(value) = action {
        match parse_compression_strategy(value) {
            Ok(strategy) => strategy,
            Err(error) => {
                println!("{}", style(error).red());
                return Ok(());
            }
        }
    } else {
        let choices = [
            "Summary — сводка + последние N сообщений",
            "Sliding Window — только последние N сообщений",
            "Sticky Facts — key-value facts + последние N сообщений",
            "Branching — независимые ветки от checkpoint",
        ];
        let selected = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Стратегия управления контекстом")
            .items(&choices)
            .default(match config.compression_strategy {
                CompressionStrategy::Summary => 0,
                CompressionStrategy::SlidingWindow => 1,
                CompressionStrategy::StickyFacts => 2,
                CompressionStrategy::Branching => 3,
            })
            .interact_opt()?;
        let Some(selected) = selected else {
            println!("{}", style("Выбор стратегии отменён.").dim());
            return Ok(());
        };
        [
            CompressionStrategy::Summary,
            CompressionStrategy::SlidingWindow,
            CompressionStrategy::StickyFacts,
            CompressionStrategy::Branching,
        ][selected]
    };

    let count = if strategy == CompressionStrategy::Branching {
        config.context_messages
    } else if let Some(value) = parts.next() {
        match parse_context_messages(value) {
            Ok(0) if strategy != CompressionStrategy::Summary => {
                println!(
                    "{}",
                    style("Нулевое окно доступно только для summary (полная история).").red()
                );
                return Ok(());
            }
            Ok(value) => value,
            Err(_) => {
                println!("{}", style("Укажите целое число от 1 до 1000.").red());
                return Ok(());
            }
        }
    } else {
        Input::<usize>::with_theme(&ColorfulTheme::default())
            .with_prompt("Сколько последних сообщений оставлять (0 в summary — полная история)")
            .default(config.context_messages)
            .validate_with(move |value: &usize| -> std::result::Result<(), String> {
                match parse_context_messages(&value.to_string()) {
                    Ok(0) if strategy != CompressionStrategy::Summary => {
                        Err("нулевое окно доступно только для summary".to_owned())
                    }
                    Ok(_) => Ok(()),
                    Err(_) => Err("нужно число от 0 до 1000".to_owned()),
                }
            })
            .interact_text()?
    };
    config.compression_strategy = strategy;
    config.context_messages = count;
    config.save(config_path)?;
    for agent in &mut agents.agents {
        agent.set_compression(strategy, count);
    }
    println!(
        "Стратегия контекста: {strategy}{}.",
        if strategy == CompressionStrategy::Branching {
            "".to_owned()
        } else {
            format!(", окно {count}")
        }
    );
    if strategy == CompressionStrategy::Branching {
        println!(
            "Используйте /checkpoint, отправьте первое сообщение ветки, затем /load для создания следующей ветки."
        );
    }
    Ok(())
}

pub(crate) fn format_temperature(temperature: f64) -> String {
    format!("{temperature:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

pub(crate) fn choose_temperature(
    provider: Provider,
    model: &str,
    current: f64,
) -> Result<Option<f64>> {
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

pub(crate) fn temperature_maximum(provider: Provider, model: &str) -> f64 {
    match provider {
        Provider::Claude => 1.0,
        Provider::Openai if is_original_gpt5_model(model) => 1.0,
        Provider::Openai => 2.0,
    }
}

pub(crate) fn is_original_gpt5_model(model: &str) -> bool {
    model == "gpt-5"
        || model.starts_with("gpt-5-")
        || model.starts_with("gpt-5-mini")
        || model.starts_with("gpt-5-nano")
}

pub(crate) fn supports_temperature_with_reasoning_none(model: &str) -> bool {
    model.starts_with("gpt-5.1")
        || model.starts_with("gpt-5.2")
        || model.starts_with("gpt-5.3")
        || model.starts_with("gpt-5.4")
        || model.starts_with("gpt-5.5")
        || model.starts_with("gpt-5.6")
}

pub(crate) fn normalized_temperature(provider: Provider, model: &str, current: f64) -> f64 {
    if provider == Provider::Openai && is_original_gpt5_model(model) {
        1.0
    } else {
        current.min(temperature_maximum(provider, model))
    }
}

pub(crate) async fn choose_model(
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

pub(crate) async fn fetch_models(
    client: &Client,
    config: &Config,
    provider: Provider,
) -> Result<Vec<String>> {
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

pub(crate) fn parse_model_ids(body: &Value, provider: Provider) -> Result<Vec<String>> {
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

pub(crate) fn model_supports_responses_api(provider: Provider, model: &str) -> bool {
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

pub(crate) fn mode_name(config: &ModesConfig, active_mode: Option<usize>) -> &str {
    active_mode
        .and_then(|index| config.modes.get(index))
        .map(|mode| mode.name.as_str())
        .unwrap_or("Без ограничений")
}

pub(crate) fn choose_mode(
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

pub(crate) fn create_mode() -> Result<ResponseMode> {
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

pub(crate) fn prompt_multiline(prompt: &str) -> Result<String> {
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

pub(crate) fn choose_provider() -> Result<Provider> {
    let choices = Provider::all();
    let names: Vec<String> = choices.iter().map(ToString::to_string).collect();
    let selected = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Выберите API-провайдера")
        .items(&names)
        .default(0)
        .interact()?;
    Ok(choices[selected])
}

pub(crate) fn authorize_if_needed(
    config: &mut Config,
    provider: Provider,
    path: &Path,
) -> Result<()> {
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

pub(crate) fn choose_auth_method() -> Result<AuthMethod> {
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

pub(crate) fn prompt_masked_key(prompt: &str) -> Result<String> {
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

pub(crate) fn remember_provider(
    config: &mut Config,
    provider: Provider,
    path: &Path,
) -> Result<()> {
    config.last_provider = Some(provider);
    config.save(path)
}
