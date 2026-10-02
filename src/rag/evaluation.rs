use crate::{
    agent::{Agent, AgentSettings, ApiAnswer, LiveRequestClient},
    config::{Config, ModesConfig, CONFIG_FILE, MODES_FILE},
    rag::{
        build_rag_prompt, retrieve_baseline, retrieve_enhanced, CandidateTrace, RetrievalConfig,
        RetrievalOutcome, RetrievalResult, RetrievalTrace, RetrievedChunk,
        NO_RELEVANT_CONTEXT_ANSWER,
    },
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

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RagEvalOptions {
    pub(crate) questions: PathBuf,
    pub(crate) output: PathBuf,
    pub(crate) retrieval: RetrievalConfig,
}

pub(crate) fn parse_rag_eval_options(args: &[String]) -> Result<RagEvalOptions> {
    let mut questions = None;
    let mut output = None;
    let mut candidate_k = None;
    let mut final_k = None;
    let mut min_similarity = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .with_context(|| format!("для {flag} требуется значение"))?;
        match flag {
            "--questions" if questions.is_none() => questions = Some(PathBuf::from(value)),
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            "--candidate-k" if candidate_k.is_none() => {
                candidate_k = Some(parse_usize(flag, value)?)
            }
            "--final-k" if final_k.is_none() => final_k = Some(parse_usize(flag, value)?),
            "--min-similarity" if min_similarity.is_none() => {
                min_similarity = Some(parse_f32(flag, value)?)
            }
            "--questions" | "--output" | "--candidate-k" | "--final-k" | "--min-similarity" => {
                bail!("аргумент указан более одного раза: {flag}")
            }
            _ => bail!("неизвестный аргумент режима rag-eval: {flag}"),
        }
        index += 2;
    }
    let defaults = RetrievalConfig::default();
    let retrieval = RetrievalConfig {
        candidate_k: candidate_k.unwrap_or(defaults.candidate_k),
        final_k: final_k.unwrap_or(defaults.final_k),
        min_similarity: min_similarity.unwrap_or(defaults.min_similarity),
        context_chars: defaults.context_chars,
    }
    .validate()?;
    Ok(RagEvalOptions {
        questions: questions.context("rag-eval требует --questions <path>")?,
        output: output.context("rag-eval требует --output <path>")?,
        retrieval,
    })
}

fn parse_usize(flag: &str, value: &str) -> Result<usize> {
    value
        .parse()
        .with_context(|| format!("{flag} ожидает положительное целое число"))
}

fn parse_f32(flag: &str, value: &str) -> Result<f32> {
    value
        .parse()
        .with_context(|| format!("{flag} ожидает число"))
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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OutcomeStatus {
    Answered,
    NoRelevantContext,
}

#[derive(Debug, Serialize)]
struct AnswerResult {
    status: OutcomeStatus,
    answer: Option<String>,
    fact_coverage: f64,
    source_recall: Option<f64>,
    input_tokens: u64,
    output_tokens: u64,
    duration_ms: u128,
    generation_duration_ms: u128,
    generation_requests: usize,
    repair_requests: usize,
    has_sources: Option<bool>,
    has_quotes: Option<bool>,
    citations_valid: Option<bool>,
    metadata_valid: Option<bool>,
    quotes_exact: Option<bool>,
    sources: Vec<OwnedSourceResult>,
    retrieval: RetrievalTrace,
}

#[derive(Debug, Serialize)]
struct OwnedSourceResult {
    rank: usize,
    source: String,
    section: String,
    chunk_id: String,
    quote: String,
    similarity: f32,
    original_similarity: f32,
    rewritten_similarity: Option<f32>,
    rerank_score: Option<f32>,
}

#[derive(Debug, Serialize)]
struct QuestionComparison {
    id: String,
    question: String,
    expectation: String,
    required_fact_fragments: Vec<String>,
    expected_sources: Vec<String>,
    baseline_rag: AnswerResult,
    enhanced_rag: AnswerResult,
}

#[derive(Debug, Serialize)]
struct Aggregate {
    mean_fact_coverage: f64,
    mean_source_recall: Option<f64>,
    input_tokens: u64,
    output_tokens: u64,
    duration_ms: u128,
    generation_requests: usize,
    repair_requests: usize,
    source_presence_rate: Option<f64>,
    quote_presence_rate: Option<f64>,
    citation_validity_rate: Option<f64>,
    metadata_validity_rate: Option<f64>,
    exact_quote_rate: Option<f64>,
    no_relevant_context: usize,
}

#[derive(Debug, Serialize)]
struct EvaluationReport {
    generated_at_unix_ms: u128,
    provider: String,
    model: String,
    retrieval_settings: RetrievalConfig,
    questions: Vec<QuestionComparison>,
    baseline_rag: Aggregate,
    enhanced_rag: Aggregate,
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
            anyhow::ensure!(
                path.canonicalize()?.starts_with(root),
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
    eprintln!("[rag-eval] Старт: вопросов {total}, provider {provider}, model {}, candidate_k {}, final_k {}, min_similarity {:.2}", terminal_safe(&settings.model), options.retrieval.candidate_k, options.retrieval.final_k, options.retrieval.min_similarity);

    for (index, question) in questions.into_iter().enumerate() {
        eprintln!("\n[rag-eval] ===== Вопрос {}/{total} =====", index + 1);
        log_block("[rag-eval][question]", &question.question);

        eprintln!(
            "[rag-eval][baseline_rag] Поиск top-{} и generation...",
            options.retrieval.final_k
        );
        let baseline_started = Instant::now();
        let baseline_retrieval =
            retrieve_baseline(&root, &question.question, options.retrieval).await?;
        log_retrieved_sources("baseline_rag", &baseline_retrieval.chunks);
        let baseline_prompt = build_rag_prompt(&question.question, &baseline_retrieval.chunks)?;
        let generation_started = Instant::now();
        let mut baseline_agent = Agent::new(1, client.clone(), settings.clone());
        let baseline_answer = baseline_agent
            .ask_rag(
                &question.question,
                &baseline_prompt,
                &baseline_retrieval.chunks,
            )
            .await?;
        let baseline_rag = answered_result(
            baseline_answer,
            &question,
            baseline_retrieval,
            generation_started.elapsed().as_millis(),
            baseline_started.elapsed().as_millis(),
        );
        log_result("baseline_rag", &baseline_rag);

        eprintln!("[rag-eval][enhanced_rag] Rewrite, фильтр, reranking и generation...");
        let enhanced_started = Instant::now();
        let enhanced = retrieve_enhanced(
            &root,
            &question.question,
            client.clone(),
            Arc::new(LiveRequestClient),
            settings.clone(),
            options.retrieval,
        )
        .await?;
        let enhanced_rag = match enhanced {
            RetrievalOutcome::Retrieved(retrieval) => {
                log_retrieved_sources("enhanced_rag", &retrieval.chunks);
                let prompt = build_rag_prompt(&question.question, &retrieval.chunks)?;
                let generation_started = Instant::now();
                let mut agent = Agent::new(1, client.clone(), settings.clone());
                let answer = agent
                    .ask_rag(&question.question, &prompt, &retrieval.chunks)
                    .await?;
                answered_result(
                    answer,
                    &question,
                    retrieval,
                    generation_started.elapsed().as_millis(),
                    enhanced_started.elapsed().as_millis(),
                )
            }
            RetrievalOutcome::NoRelevantContext(trace) => {
                no_context_result(&question, trace, enhanced_started.elapsed().as_millis())
            }
        };
        log_result("enhanced_rag", &enhanced_rag);
        comparisons.push(QuestionComparison {
            id: question.id,
            question: question.question,
            expectation: question.expectation,
            required_fact_fragments: question.required_fact_fragments,
            expected_sources: question.expected_sources,
            baseline_rag,
            enhanced_rag,
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
        retrieval_settings: options.retrieval,
        baseline_rag: aggregate(&comparisons, |item| &item.baseline_rag),
        enhanced_rag: aggregate(&comparisons, |item| &item.enhanced_rag),
        questions: comparisons,
    };
    log_aggregate("baseline_rag", &report.baseline_rag);
    log_aggregate("enhanced_rag", &report.enhanced_rag);
    let output = resolve(&root, &options.output);
    write_json_atomically(&output, &report)?;
    eprintln!("[rag-eval] Сравнение записано: {}", output.display());
    Ok(())
}

fn answered_result(
    answer: ApiAnswer,
    question: &ControlQuestion,
    retrieval: RetrievalResult,
    generation_duration_ms: u128,
    duration_ms: u128,
) -> AnswerResult {
    let trace = retrieval.trace;
    let sources = owned_sources(&answer.rag_citations, &retrieval.chunks);
    let source_recall = source_recall(
        &question.expected_sources,
        answer
            .rag_citations
            .iter()
            .map(|citation| citation.source.as_str()),
    );
    AnswerResult {
        status: OutcomeStatus::Answered,
        fact_coverage: fact_coverage(&answer.text, &question.required_fact_fragments),
        source_recall,
        answer: Some(answer.text),
        input_tokens: answer.input_tokens + trace.rewrite_input_tokens + trace.embedding_tokens,
        output_tokens: answer.output_tokens + trace.rewrite_output_tokens,
        duration_ms,
        generation_duration_ms,
        generation_requests: answer.generation_requests,
        repair_requests: answer.repair_requests,
        has_sources: Some(!sources.is_empty()),
        has_quotes: Some(sources.iter().all(|source| !source.quote.trim().is_empty())),
        citations_valid: Some(true),
        metadata_valid: Some(true),
        quotes_exact: Some(true),
        sources,
        retrieval: trace,
    }
}

fn no_context_result(
    question: &ControlQuestion,
    trace: RetrievalTrace,
    duration_ms: u128,
) -> AnswerResult {
    AnswerResult {
        status: OutcomeStatus::NoRelevantContext,
        answer: Some(NO_RELEVANT_CONTEXT_ANSWER.to_owned()),
        fact_coverage: 0.0,
        source_recall: (!question.expected_sources.is_empty()).then_some(0.0),
        input_tokens: trace.rewrite_input_tokens + trace.embedding_tokens,
        output_tokens: trace.rewrite_output_tokens,
        duration_ms,
        generation_duration_ms: 0,
        generation_requests: 0,
        repair_requests: 0,
        has_sources: None,
        has_quotes: None,
        citations_valid: None,
        metadata_valid: None,
        quotes_exact: None,
        sources: Vec::new(),
        retrieval: trace,
    }
}

fn owned_sources(
    citations: &[crate::rag::RagCitation],
    chunks: &[RetrievedChunk],
) -> Vec<OwnedSourceResult> {
    citations
        .iter()
        .map(|citation| {
            let chunk = &chunks[citation.id - 1];
            OwnedSourceResult {
                rank: citation.id,
                source: citation.source.clone(),
                section: citation.section.clone(),
                chunk_id: citation.chunk_id.clone(),
                quote: citation.quote.clone(),
                similarity: chunk.similarity,
                original_similarity: chunk.original_similarity,
                rewritten_similarity: chunk.rewritten_similarity,
                rerank_score: chunk.rerank_score,
            }
        })
        .collect()
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

fn log_retrieved_sources(mode: &str, chunks: &[RetrievedChunk]) {
    for (index, chunk) in chunks.iter().enumerate() {
        let rerank = chunk
            .rerank_score
            .map(|value| format!(" · rerank {value:.4}"))
            .unwrap_or_default();
        eprintln!(
            "[rag-eval][{mode}][source {}] {} — {} · similarity {:.4}{rerank}",
            index + 1,
            terminal_safe(&chunk.source),
            terminal_safe(&chunk.section),
            chunk.similarity
        );
    }
}

fn log_result(mode: &str, answer: &AnswerResult) {
    if let Some(text) = &answer.answer {
        log_block(&format!("[rag-eval][{mode}][agent]"), text);
    }
    let source_recall = answer.source_recall.map_or_else(
        || "n/a".to_owned(),
        |value| format!("{:.1}%", value * 100.0),
    );
    eprintln!("[rag-eval][{mode}][metrics] status {:?} · facts {:.1}% · sources {source_recall} · tokens {} in / {} out · {} ms", answer.status, answer.fact_coverage * 100.0, answer.input_tokens, answer.output_tokens, answer.duration_ms);
}

fn log_aggregate(mode: &str, aggregate: &Aggregate) {
    let source_recall = aggregate.mean_source_recall.map_or_else(
        || "n/a".to_owned(),
        |value| format!("{:.1}%", value * 100.0),
    );
    eprintln!("[rag-eval][summary][{mode}] facts {:.1}% · sources {source_recall} · tokens {} in / {} out · {} ms · no_context {}", aggregate.mean_fact_coverage * 100.0, aggregate.input_tokens, aggregate.output_tokens, aggregate.duration_ms, aggregate.no_relevant_context);
}

fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
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
    facts
        .iter()
        .filter(|fact| answer.contains(&normalized(fact)))
        .count() as f64
        / facts.len() as f64
}
fn source_recall<'a>(
    expected: &[String],
    sources: impl IntoIterator<Item = &'a str>,
) -> Option<f64> {
    if expected.is_empty() {
        return None;
    }
    let found = sources.into_iter().collect::<HashSet<_>>();
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
        generation_requests: answers
            .iter()
            .map(|answer| answer.generation_requests)
            .sum(),
        repair_requests: answers.iter().map(|answer| answer.repair_requests).sum(),
        source_presence_rate: mean_optional_bool(&answers, |answer| answer.has_sources),
        quote_presence_rate: mean_optional_bool(&answers, |answer| answer.has_quotes),
        citation_validity_rate: mean_optional_bool(&answers, |answer| answer.citations_valid),
        metadata_validity_rate: mean_optional_bool(&answers, |answer| answer.metadata_valid),
        exact_quote_rate: mean_optional_bool(&answers, |answer| answer.quotes_exact),
        no_relevant_context: answers
            .iter()
            .filter(|answer| answer.status == OutcomeStatus::NoRelevantContext)
            .count(),
    }
}

fn mean_optional_bool(
    answers: &[&AnswerResult],
    select: impl Fn(&AnswerResult) -> Option<bool>,
) -> Option<f64> {
    let values = answers
        .iter()
        .filter_map(|answer| select(answer))
        .collect::<Vec<_>>();
    (!values.is_empty())
        .then(|| values.iter().filter(|value| **value).count() as f64 / values.len() as f64)
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
    use std::sync::Mutex;

    #[test]
    fn rag_eval_options_require_unique_known_valid_flags() {
        let options = parse_rag_eval_options(&[
            "--questions".into(),
            "q.json".into(),
            "--output".into(),
            "o.json".into(),
            "--candidate-k".into(),
            "12".into(),
            "--final-k".into(),
            "4".into(),
            "--min-similarity".into(),
            "0.5".into(),
        ])
        .unwrap();
        assert_eq!(
            (options.retrieval.candidate_k, options.retrieval.final_k),
            (12, 4)
        );
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
        assert!(parse_rag_eval_options(&[
            "--questions".into(),
            "q".into(),
            "--output".into(),
            "o".into(),
            "--candidate-k".into(),
            "1".into(),
            "--final-k".into(),
            "2".into()
        ])
        .is_err());
        assert!(parse_rag_eval_options(&[
            "--questions".into(),
            "q".into(),
            "--output".into(),
            "o".into(),
            "--min-similarity".into(),
            "NaN".into()
        ])
        .is_err());
    }

    fn trace() -> RetrievalTrace {
        RetrievalTrace {
            rewritten_query: Some("rewrite".into()),
            settings: RetrievalConfig::default(),
            candidates_before_filter: 1,
            candidates_after_filter: 0,
            rewrite_input_tokens: 2,
            rewrite_output_tokens: 1,
            rewrite_duration_ms: 3,
            embedding_tokens: 4,
            embedding_duration_ms: 5,
            before_filter: vec![CandidateTrace {
                rank: 1,
                chunk_id: "c".into(),
                source: "a.md".into(),
                title: "A".into(),
                section: "S".into(),
                original_similarity: 0.2,
                rewritten_similarity: Some(0.3),
                semantic_score: 0.3,
                rerank_score: None,
            }],
            after_filter: Vec::new(),
        }
    }

    #[test]
    fn no_context_is_serialized_with_refusal_and_without_sources() {
        let question = ControlQuestion {
            id: "q".into(),
            question: "Q".into(),
            expectation: "E".into(),
            required_fact_fragments: vec!["fact".into()],
            expected_sources: vec!["a.md".into()],
        };
        let result = no_context_result(&question, trace(), 8);
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["status"], "no_relevant_context");
        assert!(value["answer"].as_str().unwrap().contains("Не знаю"));
        assert_eq!(value["fact_coverage"], 0.0);
        assert_eq!(value["sources"].as_array().unwrap().len(), 0);
        assert_eq!(value["input_tokens"], 6);
        assert!(value["has_sources"].is_null());
        assert_eq!(value["generation_requests"], 0);
    }

    #[test]
    fn answered_result_serializes_grounding_checks_and_repair_metrics() {
        let question = ControlQuestion {
            id: "q".into(),
            question: "Q".into(),
            expectation: "E".into(),
            required_fact_fragments: vec!["факт".into()],
            expected_sources: vec!["doc.md".into()],
        };
        let chunk = RetrievedChunk {
            chunk_id: "c1".into(),
            source: "doc.md".into(),
            title: "Doc".into(),
            section: "Section".into(),
            content: "точная цитата".into(),
            similarity: 0.9,
            original_similarity: 0.8,
            rewritten_similarity: Some(0.9),
            rerank_score: Some(0.85),
        };
        let mut retrieval_trace = trace();
        retrieval_trace.candidates_after_filter = 1;
        let retrieval = RetrievalResult {
            chunks: vec![chunk],
            trace: retrieval_trace,
        };
        let answer = ApiAnswer {
            text: "факт [1]\n\nИсточники:\n[1] doc.md".into(),
            task_update: None,
            task_update_warning: None,
            input_tokens: 10,
            output_tokens: 5,
            session_input_tokens: 10,
            session_output_tokens: 5,
            tool_calls: Vec::new(),
            rag_citations: vec![crate::rag::RagCitation {
                id: 1,
                context_id: 1,
                source: "doc.md".into(),
                section: "Section".into(),
                chunk_id: "c1".into(),
                quote: "точная цитата".into(),
            }],
            generation_requests: 2,
            repair_requests: 1,
        };
        let result = answered_result(answer, &question, retrieval, 12, 20);
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["has_sources"], true);
        assert_eq!(value["has_quotes"], true);
        assert_eq!(value["citations_valid"], true);
        assert_eq!(value["metadata_valid"], true);
        assert_eq!(value["quotes_exact"], true);
        assert_eq!(value["generation_requests"], 2);
        assert_eq!(value["repair_requests"], 1);
        assert_eq!(value["sources"][0]["chunk_id"], "c1");
        assert_eq!(value["sources"][0]["quote"], "точная цитата");
        assert_eq!(result.source_recall, Some(1.0));
        assert_eq!(
            mean_optional_bool(&[&result], |answer| answer.has_sources),
            Some(1.0)
        );
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
        assert_eq!(source_recall(&[], std::iter::empty()), None);
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
        assert_eq!(
            load_control_questions(&root, Path::new("reports/day22/control-questions.json"))
                .unwrap()
                .len(),
            10
        );
    }

    #[test]
    fn day24_control_questions_are_valid() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            load_control_questions(&root, Path::new("reports/day24/control-questions.json"))
                .unwrap()
                .len(),
            10
        );
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
                    text: "ответ [1]\n\n<RAG_ATTRIBUTION>\ncitations[1]{id,context_id,source,section,chunk_id,quote}:\n  1,1,\"doc.md\",\"Section\",\"c\",\"context\"\n</RAG_ATTRIBUTION>".into(),
                    task_update: None,
                    task_update_warning: None,
                    input_tokens: 1,
                    output_tokens: 1,
                    session_input_tokens: 0,
                    session_output_tokens: 0,
                    tool_calls: Vec::new(),
                    rag_citations: Vec::new(),
                    generation_requests: 1,
                    repair_requests: 0,
                })
            })
        }
    }

    #[tokio::test]
    async fn generation_pairs_use_fresh_agent_contexts() {
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
        let chunk = RetrievedChunk {
            chunk_id: "c".into(),
            source: "doc.md".into(),
            title: "Doc".into(),
            section: "Section".into(),
            content: "context".into(),
            similarity: 1.0,
            original_similarity: 1.0,
            rewritten_similarity: None,
            rerank_score: None,
        };
        for index in 0..10 {
            let question = format!("question-{index}");
            let prompt = build_rag_prompt(&question, std::slice::from_ref(&chunk)).unwrap();
            for _ in 0..2 {
                let mut agent = Agent::new(1, Client::new(), settings.clone());
                agent.request_client = recorder.clone();
                agent
                    .ask_rag(&question, &prompt, std::slice::from_ref(&chunk))
                    .await
                    .unwrap();
            }
        }
        let histories = recorder.histories.lock().unwrap();
        assert_eq!(histories.len(), 20);
        assert!(histories.iter().all(|history| history.len() == 1));
    }
}
