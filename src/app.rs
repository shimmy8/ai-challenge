#![allow(unused_imports)]
use crate::{
    agent::*, cli::*, config::*, mcp::*, memory::*, metrics::*, model::*, providers::*, sessions::*,
};
use anyhow::{anyhow, bail, Context, Result};
use console::{style, Key, Term};
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, Select};
use reqwest::{Client, StatusCode};
use rusqlite::{params, Connection, OptionalExtension};
use rustyline::{error::ReadlineError, history::DefaultHistory, Editor};
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
pub(crate) async fn run() -> Result<()> {
    let startup_mode = parse_startup_mode(std::env::args().skip(1))?;
    if startup_mode == StartupMode::McpServer {
        return run_mcp_server().await;
    }
    let StartupMode::Interactive { dump_metrics } = startup_mode else {
        unreachable!();
    };
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
    agents.set_invariants(sessions.list_invariants()?);
    agents.set_memory(ActiveMemory {
        long_term_facts: sessions.list_memory_entries()?,
        ..ActiveMemory::default()
    });
    let branching_commands_enabled = Arc::new(AtomicBool::new(
        config.compression_strategy == CompressionStrategy::Branching,
    ));
    let mut editor = Editor::<CommandHelper, DefaultHistory>::new()?;
    editor.set_helper(Some(CommandHelper::new(branching_commands_enabled.clone())));
    loop {
        let displayed_memory = agents.memory();
        show_status_bar(StatusBar {
            provider,
            model: config.model(provider)?,
            mode: mode_name(&modes, active_mode),
            temperature: config.temperature(provider)?,
            strategy: config.compression_strategy,
            context_messages: config.context_messages,
            active_branch: agents.agents.first().map(|agent| {
                if agent.branch_pending {
                    "checkpoint"
                } else {
                    agent.active_branch.as_str()
                }
            }),
            memory: &displayed_memory,
        })?;
        let prompt = format!("{} ", style("Вы ›").green().bold());
        let readline_result = editor.readline(&prompt);
        clear_status_bar(displayed_memory.task.is_some())?;
        let input = match readline_result {
            Ok(value) => value.trim().to_owned(),
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => break,
            Err(err) => return Err(err.into()),
        };
        if input.is_empty() {
            continue;
        }
        let input = expand_command_hint(
            &input,
            config.compression_strategy == CompressionStrategy::Branching,
        )
        .to_owned();
        let _ = editor.add_history_entry(&input);
        let mut automatic_input = None;
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
                config.compression_strategy = session.compression_strategy;
                config.context_messages = session.context_messages;
                active_mode = session
                    .mode
                    .as_deref()
                    .and_then(|name| modes.modes.iter().position(|mode| mode.name == name));
                config.last_mode = active_mode.map(|index| modes.modes[index].name.clone());
                config.save(&config_path)?;
                branching_commands_enabled.store(
                    config.compression_strategy == CompressionStrategy::Branching,
                    Ordering::Relaxed,
                );
                agents.reconfigure(AgentSettings::from_config(
                    &config,
                    provider,
                    active_mode.and_then(|index| modes.modes.get(index)),
                )?);
                agents.restore(session.messages);
                for agent in &mut agents.agents {
                    agent.facts = session.facts.clone();
                    if agent.settings.compression_strategy == CompressionStrategy::Summary {
                        agent.summary = session.summary.clone();
                        agent.history.drain(..session.summarized_count);
                        if agent.history.len()
                            < agent
                                .settings
                                .context_messages
                                .min(agent.persisted_history.len())
                        {
                            agent.history = agent.persisted_history.clone();
                            agent.summary.clear();
                        }
                    }
                    agent.branches = session.branches.clone();
                    agent.checkpoint = session.checkpoint.clone();
                    agent.active_branch = session.active_branch.clone();
                    agent.branch_pending = session.branch_pending;
                    if agent.settings.compression_strategy == CompressionStrategy::Branching {
                        let state = if agent.branch_pending {
                            agent.checkpoint.clone()
                        } else {
                            agent.branches.get(&agent.active_branch).cloned()
                        };
                        if let Some(state) = state {
                            agent.restore_state(state);
                        }
                    }
                }
                agents.set_memory(ActiveMemory {
                    profile: session.profile.clone(),
                    task: session.task.clone(),
                    long_term_facts: sessions.list_memory_entries()?,
                });
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
                let memory = agents.memory();
                provider = choose_provider()?;
                authorize_if_needed(&mut config, provider, &config_path)?;
                remember_provider(&mut config, provider, &config_path)?;
                agents.reconfigure(AgentSettings::from_config(
                    &config,
                    provider,
                    active_mode.and_then(|index| modes.modes.get(index)),
                )?);
                agents.set_memory(memory);
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
                let memory = agents.memory();
                agents.reconfigure(AgentSettings::from_config(
                    &config,
                    provider,
                    active_mode.and_then(|index| modes.modes.get(index)),
                )?);
                agents.set_memory(memory);
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
                let memory = agents.memory();
                agents.reconfigure(AgentSettings::from_config(
                    &config,
                    provider,
                    active_mode.and_then(|index| modes.modes.get(index)),
                )?);
                agents.set_memory(memory);
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
                let memory = agents.memory();
                agents.reconfigure(AgentSettings::from_config(
                    &config,
                    provider,
                    active_mode.and_then(|index| modes.modes.get(index)),
                )?);
                agents.set_memory(memory);
                active_session_id = None;
                println!(
                    "{} {}. {}\n",
                    style("Температура изменена на").yellow(),
                    style(format_temperature(temperature)).cyan().bold(),
                    style("Начата новая сессия.").dim()
                );
                continue;
            }
            "/mcp" => {
                if let Err(error) = handle_mcp_command(&mut config, &config_path).await {
                    eprintln!("{} {error:#}", style("Команда MCP не выполнена:").red());
                }
                continue;
            }
            command if command == "/profile" || command.starts_with("/profile ") => {
                let current = agents.memory();
                match handle_profile_command(command, &sessions, current.profile.as_ref())? {
                    ProfileCommandOutcome::Unchanged => {}
                    ProfileCommandOutcome::Select(profile) => {
                        if current.profile.as_ref().map(|value| value.id)
                            != profile.as_ref().map(|value| value.id)
                        {
                            let mut memory = current;
                            memory.profile = profile;
                            let started = activate_memory_context(
                                &mut agents,
                                &mut active_session_id,
                                memory,
                            );
                            println!(
                                "{}{}",
                                style("Профиль обновлён.").yellow(),
                                if started {
                                    format!(" {}", style("Начата новая сессия.").dim())
                                } else {
                                    String::new()
                                }
                            );
                        }
                    }
                }
                continue;
            }
            command if command == "/task" || command.starts_with("/task ") => {
                let current = agents.memory();
                let outcome = match handle_task_command(command, &sessions, current.task.as_ref()) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        eprintln!("{} {error:#}", style("Команда задачи не выполнена:").red());
                        continue;
                    }
                };
                let automatic_instruction = match outcome {
                    TaskCommandOutcome::Unchanged => None,
                    TaskCommandOutcome::Select(task) => {
                        if current.task.as_ref().map(|value| value.id)
                            != task.as_ref().map(|value| value.id)
                        {
                            let mut memory = current;
                            memory.task = task;
                            let started = activate_memory_context(
                                &mut agents,
                                &mut active_session_id,
                                memory,
                            );
                            println!(
                                "{}{}",
                                style("Активная задача обновлена.").yellow(),
                                if started {
                                    format!(" {}", style("Начата новая сессия.").dim())
                                } else {
                                    String::new()
                                }
                            );
                        } else if current.task.as_ref() != task.as_ref() {
                            let mut memory = current;
                            memory.task = task;
                            agents.set_memory(memory);
                        }
                        None
                    }
                    TaskCommandOutcome::Refresh(task) => {
                        let phase = task.phase;
                        let mut memory = current;
                        memory.task = Some(task);
                        agents.set_memory(memory);
                        task_phase_start_instruction(phase)
                    }
                };
                let Some(instruction) = automatic_instruction else {
                    continue;
                };
                automatic_input = Some(instruction.to_owned());
            }
            command if command == "/remember" || command.starts_with("/remember ") => {
                if let Some(entry) = handle_remember_command(command, &sessions)? {
                    let mut memory = agents.memory();
                    memory.long_term_facts = sessions.list_memory_entries()?;
                    agents.set_memory(memory);
                    println!(
                        "{} #{}: {}",
                        style("Долговременный факт сохранён.").yellow(),
                        entry.id,
                        entry.content
                    );
                }
                continue;
            }
            command if command == "/invariant" || command.starts_with("/invariant ") => {
                match handle_invariant_command(command, &sessions) {
                    Ok(true) => agents.set_invariants(sessions.list_invariants()?),
                    Ok(false) => {}
                    Err(error) => eprintln!("{} {error:#}", style("Ошибка инварианта:").red()),
                }
                continue;
            }
            command if command == "/forget" || command.starts_with("/forget ") => {
                if handle_forget_command(command, &sessions)? {
                    let mut memory = agents.memory();
                    memory.long_term_facts = sessions.list_memory_entries()?;
                    agents.set_memory(memory);
                    println!("{}", style("Долговременный факт удалён.").yellow());
                }
                continue;
            }
            "/memory" => {
                println!(
                    "{}\n",
                    format_memory(&agents.memory(), agents.persisted_history().len())
                );
                continue;
            }
            command
                if command == "/compression"
                    || command.starts_with("/compression ")
                    || command == "/compresson"
                    || command.starts_with("/compresson ") =>
            {
                let arguments = command
                    .strip_prefix("/compression")
                    .or_else(|| command.strip_prefix("/compresson"))
                    .unwrap()
                    .trim();
                let previous_compression = (config.compression_strategy, config.context_messages);
                handle_compression_command(arguments, &mut config, &config_path, &mut agents)?;
                if previous_compression != (config.compression_strategy, config.context_messages) {
                    if let Some(session_id) = active_session_id {
                        sessions.update_compression(
                            session_id,
                            config.compression_strategy,
                            config.context_messages,
                        )?;
                        if config.compression_strategy == CompressionStrategy::Branching {
                            sessions.save_branching_state(session_id, &agents.agents[0])?;
                        }
                    }
                }
                branching_commands_enabled.store(
                    config.compression_strategy == CompressionStrategy::Branching,
                    Ordering::Relaxed,
                );
                continue;
            }
            command
                if config.compression_strategy == CompressionStrategy::Branching
                    && (command == "/checkpoint"
                        || command == "/load"
                        || command == "/switch"
                        || command == "/branches"
                        || command.starts_with("/switch ")) =>
            {
                handle_branching_command(command, &mut agents)?;
                if let Some(session_id) = active_session_id {
                    sessions.save_branching_state(session_id, &agents.agents[0])?;
                }
                continue;
            }
            "/help" => {
                print_help(config.compression_strategy == CompressionStrategy::Branching);
                continue;
            }
            command if command.starts_with('/') => {
                println!("{}", style("Неизвестная команда. Используйте /help.").red());
                continue;
            }
            _ => {}
        }
        let mut request_input = automatic_input.unwrap_or(input);
        loop {
            print!("{} ", style("● Агент 1 · выполняется…").yellow());
            std::io::stdout().flush()?;
            let branch_was_pending = agents
                .agents
                .first()
                .is_some_and(|agent| agent.branch_pending);
            let mut results = agents.ask_all(&request_input).await;
            let mut task_progress = None;
            for run in &mut results {
                let Ok(answer) = &mut run.result else {
                    continue;
                };
                if let Some(progress) = persist_answer_task_update(&sessions, &mut agents, answer) {
                    task_progress = Some(progress);
                }
            }
            let answered = results.iter().any(|run| run.result.is_ok());
            if answered {
                active_session_id = Some(
                    sessions.save(
                        active_session_id,
                        SessionSnapshot {
                            provider,
                            model: config.model(provider)?,
                            mode: active_mode.and_then(|index| {
                                modes.modes.get(index).map(|mode| mode.name.as_str())
                            }),
                            temperature: config.temperature(provider)?,
                            messages: agents.persisted_history(),
                            summary: &agents.agents[0].summary,
                            facts: &agents.agents[0].facts,
                            summarized_count: agents.agents[0]
                                .persisted_history
                                .len()
                                .saturating_sub(agents.agents[0].history.len()),
                            compression_strategy: config.compression_strategy,
                            context_messages: config.context_messages,
                            branches: &agents.agents[0].branches,
                            checkpoint: agents.agents[0].checkpoint.as_ref(),
                            active_branch: &agents.agents[0].active_branch,
                            branch_pending: agents.agents[0].branch_pending,
                            profile_id: agents.memory().profile.map(|profile| profile.id),
                            task_id: agents.memory().task.map(|task| task.id),
                        },
                    )?,
                );
            }
            print!("\r{}\r", " ".repeat(60));
            if answered && branch_was_pending {
                println!(
                    "{}",
                    style(format!(
                        "Создана ветка «{}».",
                        agents.agents[0].active_branch
                    ))
                    .yellow()
                );
            }
            for run in results {
                if let Some(path) = &metrics_log_path {
                    let entry = MetricsLogEntry {
                        timestamp_unix_ms: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis(),
                        session_id: active_session_id,
                        outcome: if run.result.is_ok() {
                            "success"
                        } else {
                            "failed"
                        },
                        branch: &agents.agents[0].active_branch,
                        agent_id: run.agent_id,
                        provider,
                        model: config.model(provider)?,
                        elapsed_ms: run.elapsed.as_millis(),
                        request_input_tokens: run.input_tokens,
                        request_output_tokens: run.output_tokens,
                        session_input_tokens: run.session_input_tokens,
                        session_output_tokens: run.session_output_tokens,
                    };
                    if let Err(error) = append_metrics_log(path, &entry) {
                        eprintln!("{} {error:#}", style("Не удалось записать метрики:").red());
                    }
                }
                match run.result {
                    Ok(answer) => {
                        println!(
                            "{} {}\n{}\n",
                            style("✓").green().bold(),
                            style(format!("Агент {}", run.agent_id)).magenta().bold(),
                            answer.text
                        );
                        if let Some(warning) = &answer.task_update_warning {
                            eprintln!("{} {warning}", style("Предупреждение задачи:").yellow());
                        }
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
            match task_progress {
                Some(TaskUpdateProgress::Continue(phase)) => {
                    let Some(instruction) = task_phase_continue_instruction(phase) else {
                        break;
                    };
                    request_input = instruction.to_owned();
                }
                Some(TaskUpdateProgress::Completed(phase)) => {
                    if let Some(message) = task_phase_completion_message(phase) {
                        println!("{}\n", style(message).yellow().bold());
                    }
                    break;
                }
                None => break,
            }
        }
    }
    println!("{}", style("До встречи! 🦊").magenta());
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProfileCommandOutcome {
    Unchanged,
    Select(Option<Profile>),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TaskCommandOutcome {
    Unchanged,
    Select(Option<Task>),
    Refresh(Task),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskUpdateProgress {
    Continue(TaskPhase),
    Completed(TaskPhase),
}

pub(crate) fn persist_answer_task_update(
    store: &SessionStore,
    agents: &mut AgentPool,
    answer: &mut ApiAnswer,
) -> Option<TaskUpdateProgress> {
    let update = answer.task_update.take()?;
    let task = agents.memory().task?;
    let was_ready = task.ready_for_next();
    let phase = task.phase;
    let previous_step = task.todo.current_for(phase).map(|(_, item)| item.id);
    match store.save_task_update_with_result(task.id, task.phase, &update, Some(&answer.text)) {
        Ok(task) => {
            let became_ready = !was_ready && task.ready_for_next();
            let next_step = task.todo.current_for(phase).map(|(_, item)| item.id);
            let advanced = matches!(phase, TaskPhase::Execution | TaskPhase::Validation)
                && previous_step.is_some()
                && previous_step != next_step;
            let progress = if became_ready {
                Some(TaskUpdateProgress::Completed(phase))
            } else if advanced && next_step.is_some() {
                Some(TaskUpdateProgress::Continue(phase))
            } else {
                None
            };
            let mut memory = agents.memory();
            memory.task = Some(task);
            agents.set_memory(memory);
            progress
        }
        Err(error) => {
            let warning = format!("TODO не обновлён: {error:#}");
            answer.task_update_warning = Some(
                answer
                    .task_update_warning
                    .take()
                    .map_or(warning.clone(), |current| format!("{current}; {warning}")),
            );
            None
        }
    }
}

pub(crate) fn task_phase_completion_message(phase: TaskPhase) -> Option<String> {
    phase.next().map(|next| {
        format!("Все пункты этапа {phase} завершены. Для перехода к {next} используйте /task next.")
    })
}

pub(crate) fn task_phase_continue_instruction(phase: TaskPhase) -> Option<&'static str> {
    match phase {
        TaskPhase::Execution => Some(
            "Автоматически продолжи этап execution: выполни вычисленный текущий пункт e и сохрани его результат через TASK_UPDATE.",
        ),
        TaskPhase::Validation => Some(
            "Автоматически продолжи этап validation: выполни вычисленный текущий пункт v и сохрани его результат через TASK_UPDATE.",
        ),
        TaskPhase::Planning | TaskPhase::Done => None,
    }
}

pub(crate) fn activate_memory_context(
    agents: &mut AgentPool,
    active_session_id: &mut Option<i64>,
    memory: ActiveMemory,
) -> bool {
    let started = !agents.persisted_history().is_empty();
    if started {
        agents.reset();
        *active_session_id = None;
    }
    agents.set_memory(memory);
    started
}

pub(crate) fn handle_profile_command(
    command: &str,
    store: &SessionStore,
    current: Option<&Profile>,
) -> Result<ProfileCommandOutcome> {
    let arguments = command.strip_prefix("/profile").unwrap_or_default().trim();
    match arguments.split_once(' ').unwrap_or((arguments, "")) {
        ("show", _) => {
            match current {
                Some(profile) => println!(
                    "{} #{} «{}»\n{}",
                    style("Активный профиль:").yellow(),
                    profile.id,
                    profile.name,
                    profile.instructions
                ),
                None => println!("{}", style("Профиль не выбран.").dim()),
            }
            Ok(ProfileCommandOutcome::Unchanged)
        }
        ("off", _) => Ok(ProfileCommandOutcome::Select(None)),
        ("use", query) if !query.trim().is_empty() => Ok(ProfileCommandOutcome::Select(Some(
            find_profile(store, query.trim())?,
        ))),
        ("new", _) => create_profile_interactive(store),
        ("", _) => {
            let actions = [
                "Просмотреть активный профиль",
                "Выбрать сохранённый профиль",
                "Создать новый профиль",
                "Отключить профиль",
            ];
            let Some(action) = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Профиль персонализации")
                .items(&actions)
                .default(0)
                .interact_opt()?
            else {
                return Ok(ProfileCommandOutcome::Unchanged);
            };
            match action {
                0 => handle_profile_command("/profile show", store, current),
                1 => {
                    let profiles = store.list_profiles()?;
                    if profiles.is_empty() {
                        println!("{}", style("Сохранённых профилей пока нет.").dim());
                        return Ok(ProfileCommandOutcome::Unchanged);
                    }
                    let names = profiles
                        .iter()
                        .map(|profile| format!("#{} · {}", profile.id, profile.name))
                        .collect::<Vec<_>>();
                    let selected = Select::with_theme(&ColorfulTheme::default())
                        .with_prompt("Выберите профиль")
                        .items(&names)
                        .default(0)
                        .interact_opt()?;
                    Ok(selected.map_or(ProfileCommandOutcome::Unchanged, |index| {
                        ProfileCommandOutcome::Select(Some(profiles[index].clone()))
                    }))
                }
                2 => create_profile_interactive(store),
                _ => Ok(ProfileCommandOutcome::Select(None)),
            }
        }
        _ => bail!("используйте /profile, /profile show, /profile new, /profile use <id|имя> или /profile off"),
    }
}

fn create_profile_interactive(store: &SessionStore) -> Result<ProfileCommandOutcome> {
    let name: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Название профиля")
        .validate_with(|value: &String| {
            validate_memory_text(value, "название профиля", MEMORY_NAME_MAX_CHARS)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .interact_text()?;
    let response_style = prompt_profile_field("Стиль ответа")?;
    let response_format = prompt_profile_field("Формат ответа")?;
    let constraints = prompt_profile_field("Ограничения ответа")?;
    let instructions =
        compose_profile_instructions(&response_style, &response_format, &constraints)?;
    validate_profile(&name, &instructions)?;
    let confirmed = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("Сохранить профиль «{}»?", name.trim()))
        .default(true)
        .interact()?;
    let Some(profile) = create_profile_if_confirmed(store, &name, &instructions, confirmed)? else {
        println!("{}", style("Создание профиля отменено.").dim());
        return Ok(ProfileCommandOutcome::Unchanged);
    };
    Ok(ProfileCommandOutcome::Select(Some(profile)))
}

fn prompt_profile_field(label: &str) -> Result<String> {
    Input::with_theme(&ColorfulTheme::default())
        .with_prompt(format_profile_field_prompt(label, false))
        .validate_with(|value: &String| {
            validate_memory_text(value, label, PROFILE_INSTRUCTIONS_MAX_CHARS)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .interact_text()
        .map_err(Into::into)
}

pub(crate) fn handle_remember_command(
    command: &str,
    store: &SessionStore,
) -> Result<Option<MemoryEntry>> {
    let arguments = command.strip_prefix("/remember").unwrap_or_default().trim();
    if !arguments.is_empty() {
        return store.create_memory_entry(arguments).map(Some);
    }
    let content = prompt_multiline("Долговременный факт")?;
    validate_memory_entry(&content)?;
    let confirmed = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Сохранить факт в долговременную память?")
        .default(true)
        .interact()?;
    create_memory_entry_if_confirmed(store, &content, confirmed)
}

pub(crate) fn handle_invariant_command(command: &str, store: &SessionStore) -> Result<bool> {
    let arguments = command
        .strip_prefix("/invariant")
        .unwrap_or_default()
        .trim();
    let (action, value) = arguments.split_once(' ').unwrap_or((arguments, ""));
    match action {
        "" => choose_invariant_action(store),
        "list" if value.trim().is_empty() => {
            let rules = store.list_invariants()?;
            if rules.is_empty() {
                println!("{}", style("Глобальных инвариантов пока нет.").dim());
            } else {
                for rule in rules {
                    println!(
                        "#{} · {}: {}",
                        rule.id,
                        if rule.enabled {
                            "включён"
                        } else {
                            "выключен"
                        },
                        rule.content
                    );
                }
            }
            Ok(false)
        }
        "add" => {
            let rule = if value.trim().is_empty() {
                let content: String = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("Текст инварианта")
                    .validate_with(|text: &String| {
                        validate_invariant(text)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    })
                    .interact_text()?;
                let confirmed = Confirm::with_theme(&ColorfulTheme::default())
                    .with_prompt("Сохранить инвариант?")
                    .default(true)
                    .interact()?;
                if !confirmed {
                    println!("{}", style("Добавление инварианта отменено.").dim());
                    return Ok(false);
                }
                store.create_invariant(&content)?
            } else {
                store.create_invariant(value)?
            };
            println!(
                "{} #{}: {}",
                style("Инвариант сохранён.").yellow(),
                rule.id,
                rule.content
            );
            Ok(true)
        }
        "enable" | "disable" => {
            let id = value
                .trim()
                .parse::<i64>()
                .with_context(|| format!("используйте /invariant {action} <id>"))?;
            let rule = store.set_invariant_enabled(id, action == "enable")?;
            println!(
                "{} #{}: {}",
                style(if rule.enabled {
                    "Инвариант включён."
                } else {
                    "Инвариант выключен."
                })
                .yellow(),
                rule.id,
                rule.content
            );
            Ok(true)
        }
        "remove" => {
            let id = value
                .trim()
                .parse::<i64>()
                .context("используйте /invariant remove <id>")?;
            let rule = store.load_invariant(id)?;
            let confirmed = Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt(format!("Удалить инвариант #{}: {}?", rule.id, rule.content))
                .default(false)
                .interact()?;
            if !remove_invariant_if_confirmed(store, id, confirmed)? {
                println!("{}", style("Удаление инварианта отменено.").dim());
                return Ok(false);
            }
            println!("{}", style("Инвариант удалён.").yellow());
            Ok(true)
        }
        _ => {
            bail!("используйте /invariant, /invariant add <текст>, /invariant list, /invariant enable <id>, /invariant disable <id> или /invariant remove <id>")
        }
    }
}

fn choose_invariant_action(store: &SessionStore) -> Result<bool> {
    let actions = [
        "Просмотреть инварианты",
        "Добавить инвариант",
        "Включить инвариант",
        "Выключить инвариант",
        "Удалить инвариант",
    ];
    let Some(action) = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Глобальные инварианты")
        .items(&actions)
        .default(0)
        .interact_opt()?
    else {
        return Ok(false);
    };
    match action {
        0 => handle_invariant_command("/invariant list", store),
        1 => handle_invariant_command("/invariant add", store),
        2..=4 => {
            let rules = store.list_invariants()?;
            let candidates = rules
                .into_iter()
                .filter(|rule| match action {
                    2 => !rule.enabled,
                    3 => rule.enabled,
                    _ => true,
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                println!("{}", style("Подходящих инвариантов нет.").dim());
                return Ok(false);
            }
            let names = candidates
                .iter()
                .map(|rule| {
                    format!(
                        "#{} · {} · {}",
                        rule.id,
                        if rule.enabled {
                            "включён"
                        } else {
                            "выключен"
                        },
                        rule.content
                    )
                })
                .collect::<Vec<_>>();
            let Some(selected) = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Выберите инвариант")
                .items(&names)
                .default(0)
                .interact_opt()?
            else {
                return Ok(false);
            };
            let command = match action {
                2 => "enable",
                3 => "disable",
                _ => "remove",
            };
            handle_invariant_command(
                &format!("/invariant {command} {}", candidates[selected].id),
                store,
            )
        }
        _ => unreachable!(),
    }
}

pub(crate) fn remove_invariant_if_confirmed(
    store: &SessionStore,
    id: i64,
    confirmed: bool,
) -> Result<bool> {
    if !confirmed {
        return Ok(false);
    }
    store.delete_invariant(id)?;
    Ok(true)
}

pub(crate) fn create_memory_entry_if_confirmed(
    store: &SessionStore,
    content: &str,
    confirmed: bool,
) -> Result<Option<MemoryEntry>> {
    if confirmed {
        store.create_memory_entry(content).map(Some)
    } else {
        println!("{}", style("Сохранение факта отменено.").dim());
        Ok(None)
    }
}

pub(crate) fn handle_forget_command(command: &str, store: &SessionStore) -> Result<bool> {
    let arguments = command.strip_prefix("/forget").unwrap_or_default().trim();
    let id = arguments
        .parse::<i64>()
        .context("используйте /forget <id>")?;
    let entry = store.load_memory_entry(id)?;
    let confirmed = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("Удалить факт #{}: {}?", entry.id, entry.content))
        .default(false)
        .interact()?;
    let deleted = forget_memory_entry_if_confirmed(store, &entry, confirmed)?;
    if !deleted {
        println!("{}", style("Удаление факта отменено.").dim());
    }
    Ok(deleted)
}

pub(crate) fn forget_memory_entry_if_confirmed(
    store: &SessionStore,
    entry: &MemoryEntry,
    confirmed: bool,
) -> Result<bool> {
    if !confirmed {
        return Ok(false);
    }
    store.delete_memory_entry(entry.id)?;
    Ok(true)
}

pub(crate) fn create_profile_if_confirmed(
    store: &SessionStore,
    name: &str,
    instructions: &str,
    confirmed: bool,
) -> Result<Option<Profile>> {
    if confirmed {
        store.create_profile(name, instructions).map(Some)
    } else {
        Ok(None)
    }
}

fn find_profile(store: &SessionStore, query: &str) -> Result<Profile> {
    if let Ok(id) = query.parse::<i64>() {
        return store.load_profile(id);
    }
    store
        .list_profiles()?
        .into_iter()
        .find(|profile| profile.name == query)
        .with_context(|| format!("профиль «{query}» не найден"))
}

pub(crate) fn handle_task_command(
    command: &str,
    store: &SessionStore,
    current: Option<&Task>,
) -> Result<TaskCommandOutcome> {
    let arguments = command.strip_prefix("/task").unwrap_or_default().trim();
    let (action, value) = arguments.split_once(' ').unwrap_or((arguments, ""));
    match action {
        "show" => {
            match current {
                Some(selected) => {
                    let task = store.load_task(selected.id)?;
                    println!(
                        "{} #{} «{}»\nФаза: {}\nВерсия плана: {}\nУтверждена версия: {}\n{}\n{}",
                        style("Активная задача:").yellow(),
                        task.id,
                        task.title,
                        task.phase,
                        task.plan_version,
                        task.approved_plan_version
                            .map_or_else(|| "нет".to_owned(), |v| v.to_string()),
                        format_task_todo(&task.todo),
                        format_task_results(&task, true)
                    );
                    return Ok(TaskCommandOutcome::Select(Some(task)));
                }
                None => println!("{}", style("Задача не выбрана.").dim()),
            }
            Ok(TaskCommandOutcome::Unchanged)
        }
        "off" => Ok(TaskCommandOutcome::Select(None)),
        "use" if !value.trim().is_empty() => Ok(TaskCommandOutcome::Select(Some(find_task(
            store,
            value.trim(),
        )?))),
        "new" => create_task_interactive(store),
        "next" => {
            let selected = current.context("сначала выберите задачу через /task")?;
            let task = store.load_task(selected.id)?;
            let next = task.next_phase_if_ready()?;
            let confirmed = Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt(task_transition_prompt(&task, next))
                .default(false)
                .interact()?;
            let Some(task) = advance_task_if_confirmed(store, &task, confirmed)? else {
                println!("{}", style("Фаза задачи не изменена.").dim());
                return Ok(TaskCommandOutcome::Unchanged);
            };
            println!(
                "{} {}",
                style("Новая фаза задачи:").yellow(),
                style(task.phase).cyan().bold()
            );
            Ok(TaskCommandOutcome::Refresh(task))
        }
        "" => choose_task_action(store, current),
        _ => bail!("используйте /task, /task show, /task new, /task use <id|имя>, /task next или /task off"),
    }
}

pub(crate) fn task_transition_prompt(task: &Task, next: TaskPhase) -> String {
    let mut prompt = format!(
        "Перейти {} -> {}? Незавершённых пунктов текущего этапа: {}",
        task.phase,
        next,
        task.todo.pending_for(task.phase)
    );
    if task.phase == TaskPhase::Planning {
        prompt.push_str(&format!(
            "\nУтвердить версию плана {}:\n{}",
            task.plan_version,
            format_task_todo(&task.todo)
        ));
    }
    prompt
}

pub(crate) fn task_phase_start_instruction(phase: TaskPhase) -> Option<&'static str> {
    match phase {
        TaskPhase::Execution => Some(
            "Начни этап execution: выполни первый применимый незавершённый пункт e из TODO и после фактического выполнения отметь его ID через TASK_UPDATE.",
        ),
        TaskPhase::Validation => Some(
            "Начни этап validation: выполни первый применимый незавершённый пункт v из TODO и после полученного результата проверки отметь его ID через TASK_UPDATE.",
        ),
        TaskPhase::Planning | TaskPhase::Done => None,
    }
}

pub(crate) fn advance_task_if_confirmed(
    store: &SessionStore,
    task: &Task,
    confirmed: bool,
) -> Result<Option<Task>> {
    if confirmed {
        store
            .advance_task(task.id, task.phase, task.plan_version)
            .map(Some)
    } else {
        Ok(None)
    }
}

fn choose_task_action(store: &SessionStore, current: Option<&Task>) -> Result<TaskCommandOutcome> {
    let actions = [
        "Просмотреть активную задачу",
        "Продолжить сохранённую задачу",
        "Создать новую задачу",
        "Перейти к следующему этапу",
        "Отключить задачу",
    ];
    let Some(action) = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Рабочая память")
        .items(&actions)
        .default(0)
        .interact_opt()?
    else {
        return Ok(TaskCommandOutcome::Unchanged);
    };
    match action {
        0 => handle_task_command("/task show", store, current),
        1 => {
            let tasks = store.list_tasks()?;
            if tasks.is_empty() {
                println!("{}", style("Сохранённых задач пока нет.").dim());
                return Ok(TaskCommandOutcome::Unchanged);
            }
            let names = tasks
                .iter()
                .map(|task| format!("#{} · {} · {}", task.id, task.phase, task.title))
                .collect::<Vec<_>>();
            let selected = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Выберите задачу")
                .items(&names)
                .default(0)
                .interact_opt()?;
            Ok(selected.map_or(TaskCommandOutcome::Unchanged, |index| {
                TaskCommandOutcome::Select(Some(tasks[index].clone()))
            }))
        }
        2 => create_task_interactive(store),
        3 => handle_task_command("/task next", store, current),
        _ => Ok(TaskCommandOutcome::Select(None)),
    }
}

fn create_task_interactive(store: &SessionStore) -> Result<TaskCommandOutcome> {
    let title: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Название задачи")
        .validate_with(|value: &String| {
            validate_memory_text(value, "название задачи", TASK_TITLE_MAX_CHARS)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .interact_text()?;
    let todo = prompt_multiline("Исходное описание задачи")?;
    validate_task(&title, &todo)?;
    let confirmed = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("Создать задачу «{}»?", title.trim()))
        .default(true)
        .interact()?;
    let Some(task) = create_task_if_confirmed(store, &title, &todo, confirmed)? else {
        println!("{}", style("Создание задачи отменено.").dim());
        return Ok(TaskCommandOutcome::Unchanged);
    };
    Ok(TaskCommandOutcome::Select(Some(task)))
}

pub(crate) fn create_task_if_confirmed(
    store: &SessionStore,
    title: &str,
    todo: &str,
    confirmed: bool,
) -> Result<Option<Task>> {
    if confirmed {
        store.create_task(title, todo).map(Some)
    } else {
        Ok(None)
    }
}

fn find_task(store: &SessionStore, query: &str) -> Result<Task> {
    if let Ok(id) = query.parse::<i64>() {
        return store.load_task(id);
    }
    store
        .list_tasks()?
        .into_iter()
        .find(|task| task.title == query)
        .with_context(|| format!("задача «{query}» не найдена"))
}
