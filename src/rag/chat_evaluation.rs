use crate::{
    agent::{AgentPool, AgentSettings, ApiAnswer, LiveRequestClient},
    app::persist_answer_task_update,
    config::{Config, ModesConfig, CONFIG_FILE, MODES_FILE},
    memory::ActiveMemory,
    rag::{
        build_rag_prompt, retrieve_enhanced, RetrievalConfig, RetrievalContext, RetrievalOutcome,
        RetrievalTrace, NO_RELEVANT_CONTEXT_ANSWER,
    },
    sessions::SessionStore,
};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RagChatEvalOptions {
    pub(crate) scenarios: PathBuf,
    pub(crate) output: PathBuf,
}

pub(crate) fn parse_rag_chat_eval_options(args: &[String]) -> Result<RagChatEvalOptions> {
    let mut scenarios = None;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .with_context(|| format!("для {flag} требуется значение"))?;
        match flag {
            "--scenarios" if scenarios.is_none() => scenarios = Some(PathBuf::from(value)),
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            "--scenarios" | "--output" => bail!("аргумент указан более одного раза: {flag}"),
            _ => bail!("неизвестный аргумент режима rag-chat-eval: {flag}"),
        }
        index += 2;
    }
    Ok(RagChatEvalOptions {
        scenarios: scenarios.context("rag-chat-eval требует --scenarios <path>")?,
        output: output.context("rag-chat-eval требует --output <path>")?,
    })
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChatScenarioSet {
    pub(crate) scenarios: Vec<ChatScenario>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChatScenario {
    pub(crate) id: String,
    pub(crate) task: InitialTask,
    pub(crate) turns: Vec<ChatTurn>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InitialTask {
    pub(crate) title: String,
    pub(crate) description: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChatTurn {
    pub(crate) id: String,
    pub(crate) question: String,
    pub(crate) expected_outcome: ExpectedOutcome,
    pub(crate) required_answer_fragments: Vec<String>,
    pub(crate) required_task_title_fragments: Vec<String>,
    pub(crate) required_task_fact_fragments: Vec<String>,
    pub(crate) expected_sources: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExpectedOutcome {
    Answered,
    NoRelevantContext,
}

#[derive(Debug, Serialize)]
struct SafeSource {
    source: String,
    section: String,
    chunk_id: String,
}

#[derive(Debug, Serialize)]
struct TurnReport {
    id: String,
    question: String,
    expected_outcome: ExpectedOutcome,
    actual_outcome: Option<ExpectedOutcome>,
    answer: Option<String>,
    sources: Vec<SafeSource>,
    task_title: String,
    task_facts: Vec<String>,
    answer_fragments_ok: bool,
    task_title_ok: bool,
    task_facts_ok: bool,
    sources_ok: bool,
    sources_section_ok: bool,
    input_tokens: u64,
    output_tokens: u64,
    duration_ms: u128,
    error: Option<String>,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct ScenarioReport {
    id: String,
    passed_turns: usize,
    total_turns: usize,
    passed: bool,
    turns: Vec<TurnReport>,
}

#[derive(Debug, Serialize)]
struct ChatEvaluationReport {
    generated_at_unix_ms: u128,
    provider: String,
    model: String,
    retrieval_settings: RetrievalConfig,
    passed_scenarios: usize,
    total_scenarios: usize,
    passed: bool,
    scenarios: Vec<ScenarioReport>,
}

pub(crate) fn load_chat_scenarios(root: &Path, path: &Path) -> Result<ChatScenarioSet> {
    let resolved = resolve(root, path);
    let raw = fs::read_to_string(&resolved)
        .with_context(|| format!("не удалось прочитать {}", resolved.display()))?;
    let scenarios: ChatScenarioSet = serde_json::from_str(&raw)
        .with_context(|| format!("повреждён набор сценариев {}", resolved.display()))?;
    validate_chat_scenarios(root, &scenarios)?;
    Ok(scenarios)
}

pub(crate) fn validate_chat_scenarios(root: &Path, set: &ChatScenarioSet) -> Result<()> {
    let corpus_root = root.canonicalize()?;
    anyhow::ensure!(
        set.scenarios.len() == 2,
        "набор должен содержать ровно два сценария"
    );
    let mut scenario_ids = HashSet::new();
    for scenario in &set.scenarios {
        let label = scenario.id.trim();
        anyhow::ensure!(!label.is_empty(), "ID сценария пуст");
        anyhow::ensure!(
            scenario_ids.insert(label),
            "ID сценария повторяется: {label}"
        );
        anyhow::ensure!(
            !scenario.task.title.trim().is_empty(),
            "сценарий {label}: название задачи пусто"
        );
        anyhow::ensure!(
            !scenario.task.description.trim().is_empty(),
            "сценарий {label}: описание задачи пусто"
        );
        anyhow::ensure!(
            (10..=15).contains(&scenario.turns.len()),
            "сценарий {label}: требуется от 10 до 15 ходов"
        );
        let mut turn_ids = HashSet::new();
        for turn in &scenario.turns {
            let turn_label = turn.id.trim();
            anyhow::ensure!(!turn_label.is_empty(), "сценарий {label}: ID хода пуст");
            anyhow::ensure!(
                turn_ids.insert(turn_label),
                "сценарий {label}: ID хода повторяется: {turn_label}"
            );
            anyhow::ensure!(
                !turn.question.trim().is_empty(),
                "сценарий {label}, ход {turn_label}: вопрос пуст"
            );
            anyhow::ensure!(
                !turn.required_task_title_fragments.is_empty()
                    || !turn.required_task_fact_fragments.is_empty(),
                "сценарий {label}, ход {turn_label}: отсутствует проверка цели или памяти"
            );
            for (field, values) in [
                ("required_answer_fragments", &turn.required_answer_fragments),
                (
                    "required_task_title_fragments",
                    &turn.required_task_title_fragments,
                ),
                (
                    "required_task_fact_fragments",
                    &turn.required_task_fact_fragments,
                ),
            ] {
                anyhow::ensure!(
                    values.iter().all(|value| !value.trim().is_empty()),
                    "сценарий {label}, ход {turn_label}: {field} содержит пустое значение"
                );
            }
            match turn.expected_outcome {
                ExpectedOutcome::Answered => anyhow::ensure!(
                    !turn.expected_sources.is_empty(),
                    "сценарий {label}, ход {turn_label}: answered требует expected_sources"
                ),
                ExpectedOutcome::NoRelevantContext => anyhow::ensure!(
                    turn.expected_sources.is_empty(),
                    "сценарий {label}, ход {turn_label}: no_relevant_context требует пустые expected_sources"
                ),
            }
            for source in &turn.expected_sources {
                anyhow::ensure!(
                    source.ends_with(".md"),
                    "сценарий {label}, ход {turn_label}: источник должен быть Markdown-файлом"
                );
                let path = root.join(source);
                anyhow::ensure!(
                    path.is_file(),
                    "сценарий {label}, ход {turn_label}: источник не найден: {source}"
                );
                anyhow::ensure!(
                    path.canonicalize()?.starts_with(&corpus_root),
                    "сценарий {label}, ход {turn_label}: источник находится вне корпуса"
                );
            }
        }
    }
    Ok(())
}

pub(crate) async fn run_rag_chat_evaluation(options: RagChatEvalOptions) -> Result<()> {
    let root = std::env::current_dir()?.canonicalize()?;
    let set = load_chat_scenarios(&root, &options.scenarios)?;
    let config = Config::load(&root.join(CONFIG_FILE))?;
    let modes = ModesConfig::load(&root.join(MODES_FILE))?;
    let provider = config
        .last_provider
        .context("для rag-chat-eval сначала выберите provider в интерактивном режиме")?;
    let mode = config
        .last_mode
        .as_deref()
        .and_then(|name| modes.modes.iter().find(|mode| mode.name == name));
    let settings = AgentSettings::from_config(&config, provider, mode)?;
    let client = Client::builder().user_agent("fox-llm/0.1.0").build()?;
    let retrieval = RetrievalConfig::default();
    let mut reports = Vec::with_capacity(set.scenarios.len());

    for scenario in set.scenarios {
        eprintln!("[rag-chat-eval] Сценарий {}", safe_text(&scenario.id));
        reports.push(
            run_scenario(&root, scenario, client.clone(), settings.clone(), retrieval).await?,
        );
    }

    let passed_scenarios = reports.iter().filter(|report| report.passed).count();
    let report = ChatEvaluationReport {
        generated_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        provider: provider.to_string(),
        model: settings.model,
        retrieval_settings: retrieval,
        passed_scenarios,
        total_scenarios: reports.len(),
        passed: passed_scenarios == reports.len(),
        scenarios: reports,
    };
    let output = resolve(&root, &options.output);
    write_json_atomically(&output, &report)?;
    eprintln!("[rag-chat-eval] Отчёт записан: {}", output.display());
    Ok(())
}

async fn run_scenario(
    root: &Path,
    scenario: ChatScenario,
    client: Client,
    settings: AgentSettings,
    retrieval: RetrievalConfig,
) -> Result<ScenarioReport> {
    let db_path = std::env::temp_dir().join(format!(
        "fox-rag-chat-eval-{}-{}.db",
        std::process::id(),
        Uuid::new_v4()
    ));
    let store = SessionStore::open(&db_path)?;
    let task = store.create_task(&scenario.task.title, &scenario.task.description)?;
    let mut agents = AgentPool::new(1, client.clone(), settings.clone());
    agents.set_memory(ActiveMemory {
        task: Some(task),
        ..ActiveMemory::default()
    });
    let mut turn_reports = Vec::with_capacity(scenario.turns.len());

    let total_turns = scenario.turns.len();
    for (index, turn) in scenario.turns.into_iter().enumerate() {
        eprintln!(
            "[rag-chat-eval][{}] Ход {}/{}",
            safe_text(&scenario.id),
            index + 1,
            total_turns
        );
        let started = Instant::now();
        let memory = agents.memory();
        let context = RetrievalContext::from_state(agents.persisted_history(), &memory);
        let agent = agents
            .agents
            .first()
            .context("rag-chat-eval требует активного агента")?;
        let retrieved = retrieve_enhanced(
            root,
            &turn.question,
            agent.client.clone(),
            agent.request_client.clone(),
            agent.settings.clone(),
            context,
            retrieval,
        )
        .await;

        let execution = match retrieved {
            Ok(RetrievalOutcome::Retrieved(result)) => {
                let trace = result.trace.clone();
                let prompt = build_rag_prompt(&turn.question, &result.chunks)?;
                let mut runs = agents
                    .ask_all_rag(&turn.question, &prompt, &result.chunks)
                    .await;
                match runs.pop() {
                    Some(run) => match run.result {
                        Ok(mut answer) => {
                            persist_answer_task_update(&store, &mut agents, &mut answer);
                            Ok((ExpectedOutcome::Answered, answer, Some(trace)))
                        }
                        Err(error) => Err(error),
                    },
                    None => Err(anyhow::anyhow!("rag-chat-eval не получил результат агента")),
                }
            }
            Ok(RetrievalOutcome::NoRelevantContext(trace)) => {
                let mut runs = agents.record_all_local(&turn.question, NO_RELEVANT_CONTEXT_ANSWER);
                match runs.pop() {
                    Some(run) => run
                        .result
                        .map(|answer| (ExpectedOutcome::NoRelevantContext, answer, Some(trace))),
                    None => Err(anyhow::anyhow!(
                        "rag-chat-eval не получил локальный результат"
                    )),
                }
            }
            Err(error) => Err(error),
        };

        let report = match execution {
            Ok((outcome, answer, trace)) => assess_turn(
                turn,
                outcome,
                answer,
                trace.as_ref(),
                &agents.memory(),
                started.elapsed().as_millis(),
            ),
            Err(error) => failed_turn(turn, &agents.memory(), error, started.elapsed().as_millis()),
        };
        turn_reports.push(report);
    }

    drop(agents);
    drop(store);
    cleanup_database(&db_path);
    let passed_turns = turn_reports.iter().filter(|turn| turn.passed).count();
    Ok(ScenarioReport {
        id: scenario.id,
        passed_turns,
        total_turns: turn_reports.len(),
        passed: passed_turns == turn_reports.len(),
        turns: turn_reports,
    })
}

fn assess_turn(
    turn: ChatTurn,
    outcome: ExpectedOutcome,
    answer: ApiAnswer,
    trace: Option<&RetrievalTrace>,
    memory: &ActiveMemory,
    duration_ms: u128,
) -> TurnReport {
    let task = memory.task.as_ref();
    let task_title = task.map(|task| task.title.clone()).unwrap_or_default();
    let task_facts = task.map(|task| task.todo.facts.clone()).unwrap_or_default();
    let sources = answer
        .rag_citations
        .iter()
        .map(|citation| SafeSource {
            source: citation.source.clone(),
            section: citation.section.clone(),
            chunk_id: citation.chunk_id.clone(),
        })
        .collect::<Vec<_>>();
    let answer_fragments_ok = contains_all(&answer.text, &turn.required_answer_fragments);
    let task_title_ok = contains_all(&task_title, &turn.required_task_title_fragments);
    let task_facts_ok = turn
        .required_task_fact_fragments
        .iter()
        .all(|fragment| task_facts.iter().any(|fact| contains(fact, fragment)));
    let found = sources
        .iter()
        .map(|source| source.source.as_str())
        .collect::<HashSet<_>>();
    let sources_ok = match outcome {
        ExpectedOutcome::Answered => turn
            .expected_sources
            .iter()
            .all(|source| found.contains(source.as_str())),
        ExpectedOutcome::NoRelevantContext => sources.is_empty(),
    };
    let sources_section_ok = answer.text.contains("Источники");
    let passed = outcome == turn.expected_outcome
        && answer_fragments_ok
        && task_title_ok
        && task_facts_ok
        && sources_ok
        && sources_section_ok;
    TurnReport {
        id: turn.id,
        question: turn.question,
        expected_outcome: turn.expected_outcome,
        actual_outcome: Some(outcome),
        answer: Some(answer.text),
        sources,
        task_title,
        task_facts,
        answer_fragments_ok,
        task_title_ok,
        task_facts_ok,
        sources_ok,
        sources_section_ok,
        input_tokens: answer.input_tokens
            + trace
                .map(|value| value.rewrite_input_tokens + value.embedding_tokens)
                .unwrap_or(0),
        output_tokens: answer.output_tokens
            + trace.map(|value| value.rewrite_output_tokens).unwrap_or(0),
        duration_ms,
        error: answer.task_update_warning,
        passed,
    }
}

fn failed_turn(
    turn: ChatTurn,
    memory: &ActiveMemory,
    error: anyhow::Error,
    duration_ms: u128,
) -> TurnReport {
    TurnReport {
        id: turn.id,
        question: turn.question,
        expected_outcome: turn.expected_outcome,
        actual_outcome: None,
        answer: None,
        sources: Vec::new(),
        task_title: memory
            .task
            .as_ref()
            .map(|task| task.title.clone())
            .unwrap_or_default(),
        task_facts: memory
            .task
            .as_ref()
            .map(|task| task.todo.facts.clone())
            .unwrap_or_default(),
        answer_fragments_ok: false,
        task_title_ok: false,
        task_facts_ok: false,
        sources_ok: false,
        sources_section_ok: false,
        input_tokens: 0,
        output_tokens: 0,
        duration_ms,
        error: Some(safe_text(&error.to_string())),
        passed: false,
    }
}

fn contains(value: &str, fragment: &str) -> bool {
    value.to_lowercase().contains(&fragment.to_lowercase())
}

fn contains_all(value: &str, fragments: &[String]) -> bool {
    fragments.iter().all(|fragment| contains(value, fragment))
}

fn cleanup_database(path: &Path) {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        let _ = fs::remove_file(candidate);
    }
}

fn safe_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
}

fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

fn write_json_atomically(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("не удалось атомарно записать {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::{AgentPool, AgentSettings, ApiAnswer, CompressionStrategy},
        config::Provider,
        memory::{Task, TaskPhase, TaskTodo},
        rag::RagCitation,
    };

    fn turn(id: usize, source: &str) -> ChatTurn {
        ChatTurn {
            id: format!("t{id}"),
            question: format!("Вопрос {id}"),
            expected_outcome: ExpectedOutcome::Answered,
            required_answer_fragments: vec!["ответ".into()],
            required_task_title_fragments: vec!["цель".into()],
            required_task_fact_fragments: Vec::new(),
            expected_sources: vec![source.into()],
        }
    }

    fn valid_set(source: &str) -> ChatScenarioSet {
        ChatScenarioSet {
            scenarios: ["one", "two"]
                .into_iter()
                .map(|id| ChatScenario {
                    id: id.into(),
                    task: InitialTask {
                        title: "Цель".into(),
                        description: "Описание".into(),
                    },
                    turns: (1..=10).map(|index| turn(index, source)).collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn options_require_unique_known_paths() {
        let parsed = parse_rag_chat_eval_options(&[
            "--scenarios".into(),
            "s.json".into(),
            "--output".into(),
            "r.json".into(),
        ])
        .unwrap();
        assert_eq!(parsed.scenarios, PathBuf::from("s.json"));
        assert!(parse_rag_chat_eval_options(&[]).is_err());
        assert!(parse_rag_chat_eval_options(&[
            "--scenarios".into(),
            "a".into(),
            "--scenarios".into(),
            "b".into(),
            "--output".into(),
            "o".into(),
        ])
        .is_err());
        assert!(parse_rag_chat_eval_options(&["--unknown".into(), "x".into()]).is_err());
    }

    #[test]
    fn scenarios_validate_count_length_memory_and_sources() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("source.md"), "# Source").unwrap();
        let valid = valid_set("source.md");
        validate_chat_scenarios(root.path(), &valid).unwrap();

        let mut invalid = valid.clone();
        invalid.scenarios[0].turns.pop();
        assert!(validate_chat_scenarios(root.path(), &invalid).is_err());
        let mut invalid = valid.clone();
        invalid.scenarios[1].id = "one".into();
        assert!(validate_chat_scenarios(root.path(), &invalid).is_err());
        let mut invalid = valid.clone();
        invalid.scenarios[0].turns[0]
            .required_task_title_fragments
            .clear();
        assert!(validate_chat_scenarios(root.path(), &invalid).is_err());
        let mut invalid = valid;
        invalid.scenarios[0].turns[0].expected_outcome = ExpectedOutcome::NoRelevantContext;
        assert!(validate_chat_scenarios(root.path(), &invalid).is_err());
    }

    #[test]
    fn repository_day25_scenarios_are_valid() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        load_chat_scenarios(&root, Path::new("reports/day25/scenarios.json")).unwrap();
    }

    fn memory() -> ActiveMemory {
        ActiveMemory {
            task: Some(Task {
                id: 1,
                title: "Цель RAG".into(),
                todo: TaskTodo {
                    facts: vec!["SQLite обязателен".into()],
                    ..TaskTodo::default()
                },
                phase: TaskPhase::Planning,
                plan_version: 1,
                approved_plan_version: None,
                results: Vec::new(),
            }),
            ..ActiveMemory::default()
        }
    }

    fn settings() -> AgentSettings {
        AgentSettings {
            provider: Provider::Openai,
            api_key: Some("fake".into()),
            model: "fake".into(),
            temperature: 0.0,
            instructions: None,
            compression_strategy: CompressionStrategy::SlidingWindow,
            context_messages: 8,
        }
    }

    fn grounded_answer(source: &str) -> ApiAnswer {
        ApiAnswer {
            text: format!("факт [1]\n\nИсточники:\n[1] {source}"),
            task_update: None,
            task_update_warning: None,
            input_tokens: 2,
            output_tokens: 1,
            session_input_tokens: 2,
            session_output_tokens: 1,
            tool_calls: Vec::new(),
            rag_citations: vec![RagCitation {
                id: 1,
                context_id: 1,
                source: source.into(),
                section: "S".into(),
                chunk_id: "c".into(),
                quote: "цитата".into(),
            }],
            generation_requests: 1,
            repair_requests: 0,
        }
    }

    #[test]
    fn sequential_state_persists_inside_scenario_and_is_isolated_between_scenarios() {
        let directory = tempfile::tempdir().unwrap();
        let first_store = SessionStore::open(&directory.path().join("first.db")).unwrap();
        let second_store = SessionStore::open(&directory.path().join("second.db")).unwrap();
        let first_task = first_store
            .create_task("Первая цель", "Первый факт")
            .unwrap();
        let second_task = second_store
            .create_task("Вторая цель", "Второй факт")
            .unwrap();
        let mut first = AgentPool::new(1, Client::new(), settings());
        first.set_memory(ActiveMemory {
            task: Some(first_task),
            ..ActiveMemory::default()
        });
        first.record_all_local("Первый вопрос", NO_RELEVANT_CONTEXT_ANSWER);
        let first_context =
            RetrievalContext::from_state(first.persisted_history(), &first.memory());
        assert_eq!(first_context.recent_user_messages, ["Первый вопрос"]);
        assert_eq!(first_context.task_title.as_deref(), Some("Первая цель"));

        let mut second = AgentPool::new(1, Client::new(), settings());
        second.set_memory(ActiveMemory {
            task: Some(second_task),
            ..ActiveMemory::default()
        });
        let second_context =
            RetrievalContext::from_state(second.persisted_history(), &second.memory());
        assert!(second_context.recent_user_messages.is_empty());
        assert_eq!(second_context.task_title.as_deref(), Some("Вторая цель"));
        assert!(!second_context
            .task_facts
            .iter()
            .any(|fact| fact.contains("Первый")));
    }

    #[test]
    fn turn_assessment_requires_goal_memory_sources_and_source_section() {
        let expected = ChatTurn {
            id: "t".into(),
            question: "Q".into(),
            expected_outcome: ExpectedOutcome::Answered,
            required_answer_fragments: vec!["факт".into()],
            required_task_title_fragments: vec!["RAG".into()],
            required_task_fact_fragments: vec!["SQLite".into()],
            expected_sources: vec!["doc.md".into()],
        };
        let answer = ApiAnswer {
            text: "факт [1]\n\nИсточники:\n[1] doc.md".into(),
            task_update: None,
            task_update_warning: None,
            input_tokens: 2,
            output_tokens: 1,
            session_input_tokens: 2,
            session_output_tokens: 1,
            tool_calls: Vec::new(),
            rag_citations: vec![RagCitation {
                id: 1,
                context_id: 1,
                source: "doc.md".into(),
                section: "S".into(),
                chunk_id: "c".into(),
                quote: "цитата".into(),
            }],
            generation_requests: 1,
            repair_requests: 0,
        };
        let report = assess_turn(
            expected,
            ExpectedOutcome::Answered,
            answer,
            None,
            &memory(),
            3,
        );
        assert!(report.passed);

        let no_context_turn = ChatTurn {
            id: "n".into(),
            question: "N".into(),
            expected_outcome: ExpectedOutcome::NoRelevantContext,
            required_answer_fragments: Vec::new(),
            required_task_title_fragments: vec!["RAG".into()],
            required_task_fact_fragments: vec!["SQLite".into()],
            expected_sources: Vec::new(),
        };
        let answer = ApiAnswer {
            text: NO_RELEVANT_CONTEXT_ANSWER.into(),
            task_update: None,
            task_update_warning: None,
            input_tokens: 0,
            output_tokens: 0,
            session_input_tokens: 0,
            session_output_tokens: 0,
            tool_calls: Vec::new(),
            rag_citations: vec![RagCitation {
                id: 1,
                context_id: 1,
                source: "unexpected.md".into(),
                section: "S".into(),
                chunk_id: "c".into(),
                quote: "x".into(),
            }],
            generation_requests: 0,
            repair_requests: 0,
        };
        assert!(
            !assess_turn(
                no_context_turn,
                ExpectedOutcome::NoRelevantContext,
                answer,
                None,
                &memory(),
                1,
            )
            .passed
        );
    }

    #[test]
    fn turn_assessment_reports_lost_goal_fact_source_and_execution_error() {
        let base = || ChatTurn {
            id: "t".into(),
            question: "Q".into(),
            expected_outcome: ExpectedOutcome::Answered,
            required_answer_fragments: vec!["факт".into()],
            required_task_title_fragments: vec!["RAG".into()],
            required_task_fact_fragments: vec!["SQLite".into()],
            expected_sources: vec!["doc.md".into()],
        };

        let mut lost_goal = base();
        lost_goal.required_task_title_fragments = vec!["другая цель".into()];
        assert!(
            !assess_turn(
                lost_goal,
                ExpectedOutcome::Answered,
                grounded_answer("doc.md"),
                None,
                &memory(),
                1,
            )
            .passed
        );

        let mut lost_fact = base();
        lost_fact.required_task_fact_fragments = vec!["PostgreSQL".into()];
        assert!(
            !assess_turn(
                lost_fact,
                ExpectedOutcome::Answered,
                grounded_answer("doc.md"),
                None,
                &memory(),
                1,
            )
            .passed
        );

        let mut missing_source = base();
        missing_source.expected_sources = vec!["other.md".into()];
        assert!(
            !assess_turn(
                missing_source,
                ExpectedOutcome::Answered,
                grounded_answer("doc.md"),
                None,
                &memory(),
                1,
            )
            .passed
        );

        let failure = failed_turn(base(), &memory(), anyhow::anyhow!("provider failed"), 1);
        assert!(!failure.passed);
        assert!(failure.error.unwrap().contains("provider failed"));
    }

    #[test]
    fn atomic_report_contains_only_declared_safe_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.json");
        write_json_atomically(
            &path,
            &serde_json::json!({
                "answer": "ok",
                "sources": [{"source": "doc.md", "section": "S", "chunk_id": "c"}]
            }),
        )
        .unwrap();
        let raw = fs::read_to_string(path).unwrap();
        assert!(raw.contains("doc.md"));
        for forbidden in [
            "api_key",
            "authorization",
            "embedding",
            "content_hash",
            "prompt",
        ] {
            assert!(!raw.contains(forbidden));
        }
    }
}
