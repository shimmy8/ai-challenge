#![allow(unused_imports)]
use crate::{agent::*, cli::*, config::*, metrics::*, model::*, providers::*, sessions::*};
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
    let branching_commands_enabled = Arc::new(AtomicBool::new(
        config.compression_strategy == CompressionStrategy::Branching,
    ));
    let mut editor = Editor::<CommandHelper, DefaultHistory>::new()?;
    editor.set_helper(Some(CommandHelper::new(branching_commands_enabled.clone())));
    loop {
        show_status_bar(
            provider,
            config.model(provider)?,
            mode_name(&modes, active_mode),
            config.temperature(provider)?,
            config.compression_strategy,
            config.context_messages,
            agents.agents.first().map(|agent| {
                if agent.branch_pending {
                    "checkpoint"
                } else {
                    agent.active_branch.as_str()
                }
            }),
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
        let input = expand_command_hint(
            &input,
            config.compression_strategy == CompressionStrategy::Branching,
        )
        .to_owned();
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
        print!("{} ", style("● Агент 1 · выполняется…").yellow());
        std::io::stdout().flush()?;
        let branch_was_pending = agents
            .agents
            .first()
            .is_some_and(|agent| agent.branch_pending);
        let results = agents.ask_all(&input).await;
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
            match run.result {
                Ok(answer) => {
                    if let Some(path) = &metrics_log_path {
                        let entry = MetricsLogEntry {
                            timestamp_unix_ms: SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis(),
                            session_id: active_session_id
                                .context("успешный ответ не привязан к сессии")?,
                            branch: &agents.agents[0].active_branch,
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
    }
    println!("{}", style("До встречи! 🦊").magenta());
    Ok(())
}
