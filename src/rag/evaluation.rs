use crate::{
    agent::{Agent, AgentSettings},
    config::{Config, ModesConfig, CONFIG_FILE, MODES_FILE},
    rag::{build_rag_prompt, retrieve, RetrievedChunk},
};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RagEvalOptions {
    pub(crate) questions: PathBuf,
    pub(crate) output: PathBuf,
}

pub(crate) fn parse_rag_eval_options(args: &[String]) -> Result<RagEvalOptions> {
    let mut questions = None;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .with_context(|| format!("для {flag} требуется значение"))?;
        match flag {
            "--questions" if questions.is_none() => questions = Some(PathBuf::from(value)),
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            "--questions" | "--output" => bail!("аргумент указан более одного раза: {flag}"),
            _ => bail!("неизвестный аргумент режима rag-eval: {flag}"),
        }
        index += 2;
    }
    Ok(RagEvalOptions {
        questions: questions.context("rag-eval требует --questions <path>")?,
        output: output.context("rag-eval требует --output <path>")?,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlQuestion {
    pub(crate) id: String,
    pub(crate) question: String,
    pub(crate) expectation: String,
    pub(crate) required_fact_fragments: Vec<String>,
    pub(crate) expected_sources: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AnswerResult {
    answer: String,
    fact_coverage: f64,
    source_recall: Option<f64>,
    input_tokens: u64,
    output_tokens: u64,
    duration_ms: u128,
    sources: Vec<OwnedSourceResult>,
}

#[derive(Debug, Serialize)]
struct OwnedSourceResult {
    rank: usize,
    source: String,
    title: String,
    section: String,
    similarity: f32,
}

#[derive(Debug, Serialize)]
struct QuestionComparison {
    id: String,
    question: String,
    expectation: String,
    required_fact_fragments: Vec<String>,
    expected_sources: Vec<String>,
    plain: AnswerResult,
    rag: AnswerResult,
}

#[derive(Debug, Serialize)]
struct Aggregate {
    mean_fact_coverage: f64,
    mean_source_recall: Option<f64>,
    input_tokens: u64,
    output_tokens: u64,
    duration_ms: u128,
}

#[derive(Debug, Serialize)]
struct EvaluationReport {
    generated_at_unix_ms: u128,
    provider: String,
    model: String,
    questions: Vec<QuestionComparison>,
    plain: Aggregate,
    rag: Aggregate,
}

pub(crate) fn load_control_questions(root: &Path, path: &Path) -> Result<Vec<ControlQuestion>> {
    let resolved = resolve(root, path);
    let raw = fs::read_to_string(&resolved)
        .with_context(|| format!("не удалось прочитать {}", resolved.display()))?;
    let questions: Vec<ControlQuestion> = serde_json::from_str(&raw)
        .with_context(|| format!("повреждён контрольный набор {}", resolved.display()))?;
    validate_control_questions(root, &questions)?;
    Ok(questions)
}

pub(crate) fn validate_control_questions(root: &Path, questions: &[ControlQuestion]) -> Result<()> {
    anyhow::ensure!(
        questions.len() == 10,
        "контрольный набор должен содержать ровно 10 вопросов"
    );
    let mut ids = HashSet::new();
    for question in questions {
        anyhow::ensure!(
            !question.id.trim().is_empty(),
            "ID контрольного вопроса пуст"
        );
        anyhow::ensure!(
            ids.insert(question.id.as_str()),
            "ID контрольного вопроса повторяется"
        );
        anyhow::ensure!(
            !question.question.trim().is_empty(),
            "контрольный вопрос пуст"
        );
        anyhow::ensure!(
            !question.expectation.trim().is_empty(),
            "ожидание контрольного вопроса пусто"
        );
        anyhow::ensure!(
            question
                .required_fact_fragments
                .iter()
                .all(|item| !item.trim().is_empty()),
            "обязательный факт контрольного вопроса пуст"
        );
        for source in &question.expected_sources {
            anyhow::ensure!(
                source.ends_with(".md"),
                "ожидаемый источник должен быть Markdown-файлом"
            );
            let path = root.join(source);
            anyhow::ensure!(path.is_file(), "ожидаемый источник не найден: {source}");
            let canonical = path.canonicalize()?;
            anyhow::ensure!(
                canonical.starts_with(root),
                "ожидаемый источник находится вне корпуса"
            );
        }
    }
    Ok(())
}

pub(crate) async fn run_rag_evaluation(options: RagEvalOptions) -> Result<()> {
    let root = std::env::current_dir()?.canonicalize()?;
    let questions = load_control_questions(&root, &options.questions)?;
    let config = Config::load(&root.join(CONFIG_FILE))?;
    let modes = ModesConfig::load(&root.join(MODES_FILE))?;
    let provider = config
        .last_provider
        .context("для rag-eval сначала выберите provider в интерактивном режиме")?;
    let mode = config
        .last_mode
        .as_deref()
        .and_then(|name| modes.modes.iter().find(|mode| mode.name == name));
    let settings = AgentSettings::from_config(&config, provider, mode)?;
    let client = Client::builder().user_agent("fox-llm/0.1.0").build()?;
    let mut comparisons = Vec::with_capacity(questions.len());
    let total = questions.len();

    eprintln!(
        "[rag-eval] Старт: вопросов {total}, provider {provider}, model {}",
        terminal_safe(&settings.model)
    );

    for (index, question) in questions.into_iter().enumerate() {
        eprintln!("\n[rag-eval] ===== Вопрос {}/{total} =====", index + 1);
        log_block("[rag-eval][question]", &question.question);
        eprintln!("[rag-eval][plain] Запрос к агенту без RAG...");
        let plain_started = Instant::now();
        let mut plain_agent = Agent::new(1, client.clone(), settings.clone());
        let plain_answer = plain_agent.ask(&question.question).await?;
        let plain = answer_result(
            plain_answer.text,
            &question,
            &[],
            plain_answer.input_tokens,
            plain_answer.output_tokens,
            plain_started.elapsed().as_millis(),
        );
        log_block("[rag-eval][plain][agent]", &plain.answer);
        log_answer_metrics("plain", &plain);

        eprintln!("[rag-eval][rag] Поиск контекста и запрос к агенту...");
        let rag_started = Instant::now();
        let chunks = retrieve(&root, &question.question).await?;
        log_retrieved_sources(&chunks);
        let prompt = build_rag_prompt(&question.question, &chunks)?;
        let mut rag_agent = Agent::new(1, client.clone(), settings.clone());
        let rag_answer = rag_agent
            .ask_with_context(&question.question, Some(&prompt))
            .await?;
        let rag = answer_result(
            rag_answer.text,
            &question,
            &chunks,
            rag_answer.input_tokens,
            rag_answer.output_tokens,
            rag_started.elapsed().as_millis(),
        );
        log_block("[rag-eval][rag][agent]", &rag.answer);
        log_answer_metrics("rag", &rag);
        comparisons.push(QuestionComparison {
            id: question.id,
            question: question.question,
            expectation: question.expectation,
            required_fact_fragments: question.required_fact_fragments,
            expected_sources: question.expected_sources,
            plain,
            rag,
        });
        eprintln!("[rag-eval] Вопрос {}/{total} завершён", index + 1);
    }

    let report = EvaluationReport {
        generated_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        provider: provider.to_string(),
        model: settings.model,
        plain: aggregate(&comparisons, |item| &item.plain),
        rag: aggregate(&comparisons, |item| &item.rag),
        questions: comparisons,
    };
    log_aggregate("plain", &report.plain);
    log_aggregate("rag", &report.rag);
    let output = resolve(&root, &options.output);
    write_json_atomically(&output, &report)?;
    eprintln!("[rag-eval] Сравнение записано: {}", output.display());
    Ok(())
}

fn log_block(prefix: &str, value: &str) {
    let safe = terminal_safe(value);
    if safe.is_empty() {
        eprintln!("{prefix} <пустое сообщение>");
        return;
    }
    for line in safe.lines() {
        eprintln!("{prefix} {line}");
    }
}

fn log_retrieved_sources(chunks: &[RetrievedChunk]) {
    for (index, chunk) in chunks.iter().enumerate() {
        eprintln!(
            "[rag-eval][rag][source {}] {} — {} ({:.4})",
            index + 1,
            terminal_safe(&chunk.source),
            terminal_safe(&chunk.section),
            chunk.similarity
        );
    }
}

fn log_answer_metrics(mode: &str, answer: &AnswerResult) {
    let source_recall = answer.source_recall.map_or_else(
        || "n/a".to_owned(),
        |value| format!("{:.1}%", value * 100.0),
    );
    eprintln!(
        "[rag-eval][{mode}][metrics] facts {:.1}% · sources {source_recall} · tokens {} in / {} out · {} ms",
        answer.fact_coverage * 100.0,
        answer.input_tokens,
        answer.output_tokens,
        answer.duration_ms
    );
}

fn log_aggregate(mode: &str, aggregate: &Aggregate) {
    let source_recall = aggregate.mean_source_recall.map_or_else(
        || "n/a".to_owned(),
        |value| format!("{:.1}%", value * 100.0),
    );
    eprintln!(
        "[rag-eval][summary][{mode}] facts {:.1}% · sources {source_recall} · tokens {} in / {} out · {} ms",
        aggregate.mean_fact_coverage * 100.0,
        aggregate.input_tokens,
        aggregate.output_tokens,
        aggregate.duration_ms
    );
}

fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
}

fn answer_result(
    answer: String,
    question: &ControlQuestion,
    chunks: &[RetrievedChunk],
    input_tokens: u64,
    output_tokens: u64,
    duration_ms: u128,
) -> AnswerResult {
    AnswerResult {
        fact_coverage: fact_coverage(&answer, &question.required_fact_fragments),
        source_recall: source_recall(&question.expected_sources, chunks),
        answer,
        input_tokens,
        output_tokens,
        duration_ms,
        sources: chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| OwnedSourceResult {
                rank: index + 1,
                source: chunk.source.clone(),
                title: chunk.title.clone(),
                section: chunk.section.clone(),
                similarity: chunk.similarity,
            })
            .collect(),
    }
}

fn normalized(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn fact_coverage(answer: &str, facts: &[String]) -> f64 {
    if facts.is_empty() {
        return 1.0;
    }
    let answer = normalized(answer);
    let matched = facts
        .iter()
        .filter(|fact| answer.contains(&normalized(fact)))
        .count();
    matched as f64 / facts.len() as f64
}

fn source_recall(expected: &[String], chunks: &[RetrievedChunk]) -> Option<f64> {
    if expected.is_empty() {
        return None;
    }
    let found = chunks
        .iter()
        .map(|chunk| chunk.source.as_str())
        .collect::<HashSet<_>>();
    Some(
        expected
            .iter()
            .filter(|source| found.contains(source.as_str()))
            .count() as f64
            / expected.len() as f64,
    )
}

fn aggregate<'a>(
    comparisons: &'a [QuestionComparison],
    select: impl Fn(&'a QuestionComparison) -> &'a AnswerResult,
) -> Aggregate {
    let answers = comparisons.iter().map(select).collect::<Vec<_>>();
    let source_scores = answers
        .iter()
        .filter_map(|answer| answer.source_recall)
        .collect::<Vec<_>>();
    Aggregate {
        mean_fact_coverage: answers
            .iter()
            .map(|answer| answer.fact_coverage)
            .sum::<f64>()
            / answers.len() as f64,
        mean_source_recall: (!source_scores.is_empty())
            .then(|| source_scores.iter().sum::<f64>() / source_scores.len() as f64),
        input_tokens: answers.iter().map(|answer| answer.input_tokens).sum(),
        output_tokens: answers.iter().map(|answer| answer.output_tokens).sum(),
        duration_ms: answers.iter().map(|answer| answer.duration_ms).sum(),
    }
}

fn write_json_atomically(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
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

fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::{ApiAnswer, CompressionStrategy, RequestClient, RequestFuture},
        config::Provider,
        sessions::Message,
    };
    use std::sync::{Arc, Mutex};

    #[test]
    fn rag_eval_options_require_unique_known_flags() {
        let options = parse_rag_eval_options(&[
            "--questions".into(),
            "q.json".into(),
            "--output".into(),
            "o.json".into(),
        ])
        .unwrap();
        assert_eq!(options.questions, PathBuf::from("q.json"));
        assert!(parse_rag_eval_options(&[]).is_err());
        assert!(parse_rag_eval_options(&[
            "--questions".into(),
            "a".into(),
            "--questions".into(),
            "b".into(),
            "--output".into(),
            "o".into()
        ])
        .is_err());
        assert!(parse_rag_eval_options(&["--unknown".into(), "x".into()]).is_err());
    }

    #[test]
    fn metrics_normalize_unicode_and_handle_not_applicable_sources() {
        assert_eq!(
            fact_coverage(
                "  ЁЖИК\nВ ТУМАНЕ ",
                &["ёжик в тумане".into(), "лиса".into()]
            ),
            0.5
        );
        assert_eq!(source_recall(&[], &[]), None);
        let chunks = vec![RetrievedChunk {
            chunk_id: "1".into(),
            source: "a.md".into(),
            title: "A".into(),
            section: "S".into(),
            content: "x".into(),
            similarity: 1.0,
        }];
        assert_eq!(
            source_recall(&["a.md".into(), "b.md".into()], &chunks),
            Some(0.5)
        );
    }

    #[test]
    fn terminal_logs_remove_control_sequences_but_keep_multiline_messages() {
        assert_eq!(
            terminal_safe("Первая\n\u{1b}[31mВторая\r\nТретья\tчасть"),
            "Первая\n[31mВторая\nТретья\tчасть"
        );
    }

    #[test]
    fn day22_control_questions_are_valid() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let questions =
            load_control_questions(&root, Path::new("reports/day22/control-questions.json"))
                .unwrap();
        assert_eq!(questions.len(), 10);
    }

    #[test]
    fn validation_rejects_duplicate_ids_and_missing_sources() {
        let root = tempfile::tempdir().unwrap();
        let question = ControlQuestion {
            id: "same".into(),
            question: "Вопрос".into(),
            expectation: "Ответ".into(),
            required_fact_fragments: vec!["факт".into()],
            expected_sources: vec!["missing.md".into()],
        };
        let mut questions = vec![question; 10];
        assert!(validate_control_questions(root.path(), &questions).is_err());
        for (index, question) in questions.iter_mut().enumerate() {
            question.id = format!("q{index}");
            question.expected_sources.clear();
        }
        assert!(validate_control_questions(root.path(), &questions).is_ok());
    }

    #[test]
    fn atomic_json_write_replaces_only_after_complete_serialization() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("comparison.json");
        fs::write(&path, "old").unwrap();
        write_json_atomically(&path, &serde_json::json!({"answer": "ok"})).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("\"answer\": \"ok\""));
        assert!(!written.contains("api_key"));
        assert!(!directory
            .path()
            .join(format!("comparison.tmp-{}", std::process::id()))
            .exists());
    }

    struct RecordingClient {
        histories: Mutex<Vec<Vec<Message>>>,
    }

    impl RequestClient for RecordingClient {
        fn send<'a>(
            &'a self,
            _client: &'a Client,
            _settings: &'a AgentSettings,
            history: &'a [Message],
        ) -> RequestFuture<'a> {
            self.histories.lock().unwrap().push(history.to_vec());
            Box::pin(async {
                Ok(ApiAnswer {
                    text: "ответ".into(),
                    task_update: None,
                    task_update_warning: None,
                    input_tokens: 1,
                    output_tokens: 1,
                    session_input_tokens: 0,
                    session_output_tokens: 0,
                    tool_calls: Vec::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn evaluation_pairs_use_twenty_fresh_agent_contexts() {
        let recorder = Arc::new(RecordingClient {
            histories: Mutex::new(Vec::new()),
        });
        let settings = AgentSettings {
            provider: Provider::Openai,
            api_key: "fake".into(),
            model: "fake".into(),
            temperature: 0.0,
            instructions: None,
            compression_strategy: CompressionStrategy::Summary,
            context_messages: 10,
        };
        let chunks = vec![RetrievedChunk {
            chunk_id: "c".into(),
            source: "doc.md".into(),
            title: "Doc".into(),
            section: "Section".into(),
            content: "context".into(),
            similarity: 1.0,
        }];
        for index in 0..10 {
            let question = format!("question-{index}");
            let mut plain = Agent::new(1, Client::new(), settings.clone());
            plain.request_client = recorder.clone();
            plain.ask(&question).await.unwrap();

            let prompt = build_rag_prompt(&question, &chunks).unwrap();
            let mut rag = Agent::new(1, Client::new(), settings.clone());
            rag.request_client = recorder.clone();
            rag.ask_with_context(&question, Some(&prompt))
                .await
                .unwrap();
        }
        let histories = recorder.histories.lock().unwrap();
        assert_eq!(histories.len(), 20);
        assert!(histories.iter().all(|history| history.len() == 1));
        for (index, pair) in histories.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(pair[0][0].content, format!("question-{index}"));
            assert!(pair[1][0].content.contains(&format!("question-{index}")));
            if index > 0 {
                assert!(!pair[1][0]
                    .content
                    .contains(&format!("question-{}", index - 1)));
            }
        }
    }
}
