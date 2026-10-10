use crate::{
    agent::{AgentSettings, ApiAnswer, CompressionStrategy, LiveRequestClient, RequestClient},
    config::Provider,
    sessions::Message,
};
use anyhow::{bail, Context, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const OLLAMA_SHOW_URL: &str = "http://127.0.0.1:11434/api/show";
const OLLAMA_PS_URL: &str = "http://127.0.0.1:11434/api/ps";
const EXPECTED_BASE_MODEL: &str = "qwen3.5:4b";
const EXPECTED_FAMILY: &str = "qwen35";
const EXPECTED_PARAMETER_SIZE: &str = "4.2B";
const EXPECTED_QUANTIZATION: &str = "Q4_K_M";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SummarizeEvalOptions {
    pub(crate) dataset: PathBuf,
    pub(crate) profiles: PathBuf,
    pub(crate) output: PathBuf,
}

pub(crate) fn parse_summarize_eval_options(args: &[String]) -> Result<SummarizeEvalOptions> {
    let mut dataset = None;
    let mut profiles = None;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .with_context(|| format!("для {flag} требуется значение"))?;
        match flag {
            "--dataset" if dataset.is_none() => dataset = Some(PathBuf::from(value)),
            "--profiles" if profiles.is_none() => profiles = Some(PathBuf::from(value)),
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            "--dataset" | "--profiles" | "--output" => {
                bail!("аргумент указан более одного раза: {flag}")
            }
            _ => bail!("неизвестный аргумент режима summarize-eval: {flag}"),
        }
        index += 2;
    }
    Ok(SummarizeEvalOptions {
        dataset: dataset.context("summarize-eval требует --dataset <path>")?,
        profiles: profiles.context("summarize-eval требует --profiles <path>")?,
        output: output.context("summarize-eval требует --output <path>")?,
    })
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LengthTier {
    Short,
    Medium,
    Long,
}

impl LengthTier {
    fn as_str(self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::Medium => "medium",
            Self::Long => "long",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TranscriptBlock {
    pub(crate) text: String,
    #[serde(default = "one")]
    pub(crate) repeat: usize,
}

fn one() -> usize {
    1
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct BenchmarkDataset {
    pub(crate) version: u32,
    pub(crate) scenarios: Vec<MeetingScenario>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MeetingScenario {
    pub(crate) id: String,
    pub(crate) length_tier: LengthTier,
    pub(crate) transcript: Vec<TranscriptBlock>,
    pub(crate) expected: MeetingSummary,
    #[serde(default)]
    pub(crate) forbidden_fragments: Vec<String>,
}

impl MeetingScenario {
    pub(crate) fn rendered_transcript(&self) -> String {
        self.transcript
            .iter()
            .flat_map(|block| std::iter::repeat_n(block.text.trim(), block.repeat))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MeetingSummary {
    pub(crate) summary: String,
    pub(crate) decisions: Vec<SummaryFact>,
    pub(crate) action_items: Vec<ActionItem>,
    pub(crate) open_questions: Vec<SummaryFact>,
    pub(crate) risks: Vec<SummaryFact>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SummaryFact {
    pub(crate) id: String,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActionItem {
    pub(crate) id: String,
    pub(crate) task: String,
    pub(crate) owner: Option<String>,
    pub(crate) deadline: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromptKind {
    General,
    Specialized,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileSet {
    pub(crate) version: u32,
    pub(crate) profiles: Vec<EvaluationProfile>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvaluationProfile {
    pub(crate) id: String,
    pub(crate) model: String,
    pub(crate) temperature: f64,
    pub(crate) num_ctx: usize,
    pub(crate) num_predict: usize,
    pub(crate) prompt: PromptKind,
    pub(crate) expected_base_model: String,
    pub(crate) expected_quantization: String,
}

pub(crate) fn load_dataset(root: &Path, path: &Path) -> Result<BenchmarkDataset> {
    let resolved = resolve(root, path);
    let raw = fs::read_to_string(&resolved)
        .with_context(|| format!("не удалось прочитать {}", resolved.display()))?;
    let dataset: BenchmarkDataset = serde_json::from_str(&raw)
        .with_context(|| format!("повреждён benchmark dataset {}", resolved.display()))?;
    validate_dataset(&dataset)?;
    Ok(dataset)
}

pub(crate) fn load_profiles(root: &Path, path: &Path) -> Result<ProfileSet> {
    let resolved = resolve(root, path);
    let raw = fs::read_to_string(&resolved)
        .with_context(|| format!("не удалось прочитать {}", resolved.display()))?;
    let profiles: ProfileSet = serde_json::from_str(&raw)
        .with_context(|| format!("повреждена конфигурация профилей {}", resolved.display()))?;
    validate_profiles(&profiles)?;
    Ok(profiles)
}

pub(crate) fn validate_dataset(dataset: &BenchmarkDataset) -> Result<()> {
    anyhow::ensure!(dataset.version == 1, "неподдерживаемая версия dataset");
    anyhow::ensure!(
        dataset.scenarios.len() == 9,
        "benchmark dataset должен содержать ровно 9 сценариев"
    );
    let mut ids = HashSet::new();
    let mut tiers = BTreeMap::<LengthTier, usize>::new();
    for scenario in &dataset.scenarios {
        let id = scenario.id.trim();
        anyhow::ensure!(!id.is_empty(), "ID сценария пуст");
        anyhow::ensure!(ids.insert(id), "ID сценария повторяется: {id}");
        anyhow::ensure!(
            !scenario.transcript.is_empty()
                && scenario
                    .transcript
                    .iter()
                    .all(|block| !block.text.trim().is_empty() && (1..=200).contains(&block.repeat)),
            "сценарий {id}: протокол пуст или содержит некорректный repeat"
        );
        validate_summary(&scenario.expected, id, true)?;
        anyhow::ensure!(
            scenario
                .forbidden_fragments
                .iter()
                .all(|item| !item.trim().is_empty()),
            "сценарий {id}: forbidden_fragments содержит пустое значение"
        );
        *tiers.entry(scenario.length_tier).or_default() += 1;
    }
    for tier in [LengthTier::Short, LengthTier::Medium, LengthTier::Long] {
        anyhow::ensure!(
            tiers.get(&tier).copied() == Some(3),
            "benchmark dataset должен содержать по 3 сценария категории {}",
            tier.as_str()
        );
    }
    Ok(())
}

pub(crate) fn validate_profiles(set: &ProfileSet) -> Result<()> {
    anyhow::ensure!(set.version == 1, "неподдерживаемая версия профилей");
    anyhow::ensure!(
        set.profiles.len() == 3,
        "требуются ровно три профиля: baseline, compact и balanced"
    );
    let expected = BTreeSet::from(["baseline", "compact", "balanced"]);
    let actual = set
        .profiles
        .iter()
        .map(|profile| profile.id.as_str())
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        actual == expected,
        "набор профилей должен содержать baseline, compact и balanced"
    );
    let mut models = HashSet::new();
    for profile in &set.profiles {
        anyhow::ensure!(
            profile.temperature.is_finite() && (0.0..=2.0).contains(&profile.temperature),
            "профиль {}: temperature вне диапазона 0..=2",
            profile.id
        );
        anyhow::ensure!(
            (512..=262_144).contains(&profile.num_ctx),
            "профиль {}: num_ctx вне диапазона",
            profile.id
        );
        anyhow::ensure!(
            (1..=4096).contains(&profile.num_predict),
            "профиль {}: num_predict вне диапазона",
            profile.id
        );
        anyhow::ensure!(
            !profile.model.trim().is_empty() && models.insert(profile.model.as_str()),
            "имя alias профиля пусто или повторяется"
        );
        anyhow::ensure!(
            profile.expected_base_model == EXPECTED_BASE_MODEL,
            "профиль {}: ожидается базовая модель {EXPECTED_BASE_MODEL}",
            profile.id
        );
        anyhow::ensure!(
            profile.expected_quantization == EXPECTED_QUANTIZATION,
            "профиль {}: ожидается quantization {EXPECTED_QUANTIZATION}",
            profile.id
        );
    }
    Ok(())
}

fn validate_summary(
    summary: &MeetingSummary,
    scenario: &str,
    allow_empty_summary: bool,
) -> Result<()> {
    if !allow_empty_summary {
        anyhow::ensure!(!summary.summary.trim().is_empty(), "summary пуст");
    }
    validate_facts(&summary.decisions, scenario, "decisions")?;
    validate_facts(&summary.open_questions, scenario, "open_questions")?;
    validate_facts(&summary.risks, scenario, "risks")?;
    let mut ids = HashSet::new();
    for action in &summary.action_items {
        anyhow::ensure!(
            !action.id.trim().is_empty() && ids.insert(action.id.as_str()),
            "сценарий {scenario}: action_items содержит пустой или повторяющийся ID"
        );
        anyhow::ensure!(
            !action.task.trim().is_empty(),
            "сценарий {scenario}: action_items содержит пустую задачу"
        );
        for value in [action.owner.as_deref(), action.deadline.as_deref()]
            .into_iter()
            .flatten()
        {
            anyhow::ensure!(
                !value.trim().is_empty(),
                "сценарий {scenario}: nullable поле не может быть пустой строкой"
            );
        }
    }
    Ok(())
}

fn validate_facts(facts: &[SummaryFact], scenario: &str, field: &str) -> Result<()> {
    let mut ids = HashSet::new();
    for fact in facts {
        anyhow::ensure!(
            !fact.id.trim().is_empty() && ids.insert(fact.id.as_str()),
            "сценарий {scenario}: {field} содержит пустой или повторяющийся ID"
        );
        anyhow::ensure!(
            !fact.text.trim().is_empty(),
            "сценарий {scenario}: {field} содержит пустой текст"
        );
    }
    Ok(())
}

fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ModelIdentity {
    pub(crate) alias: String,
    pub(crate) family: String,
    pub(crate) parameter_size: Option<String>,
    pub(crate) quantization: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub(crate) struct RuntimeMetadata {
    pub(crate) context_length: Option<u64>,
    pub(crate) model_size_bytes: Option<u64>,
    pub(crate) loaded_memory_bytes: Option<u64>,
    pub(crate) quantization: Option<String>,
    pub(crate) unavailable: Vec<String>,
}

type InspectorFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub(crate) trait OllamaInspector: Send + Sync {
    fn preflight<'a>(
        &'a self,
        profile: &'a EvaluationProfile,
    ) -> InspectorFuture<'a, ModelIdentity>;
    fn runtime<'a>(&'a self, model: &'a str) -> InspectorFuture<'a, RuntimeMetadata>;
}

pub(crate) struct LiveOllamaInspector {
    client: Client,
}

impl LiveOllamaInspector {
    fn new(client: Client) -> Self {
        Self { client }
    }
}

impl OllamaInspector for LiveOllamaInspector {
    fn preflight<'a>(
        &'a self,
        profile: &'a EvaluationProfile,
    ) -> InspectorFuture<'a, ModelIdentity> {
        Box::pin(async move {
            let response = self
                .client
                .post(OLLAMA_SHOW_URL)
                .json(&json!({ "model": profile.model }))
                .send()
                .await
                .context("Ollama недоступна: запустите локальный сервер Ollama")?;
            let status = response.status();
            let body = response
                .json::<Value>()
                .await
                .context("Ollama вернула некорректный ответ /api/show")?;
            validate_show_response(profile, status, &body)
        })
    }

    fn runtime<'a>(&'a self, model: &'a str) -> InspectorFuture<'a, RuntimeMetadata> {
        Box::pin(async move {
            let response = self
                .client
                .get(OLLAMA_PS_URL)
                .send()
                .await
                .context("не удалось получить runtime metadata Ollama")?;
            let status = response.status();
            let body = response
                .json::<Value>()
                .await
                .context("Ollama вернула некорректный ответ /api/ps")?;
            parse_ps_response(model, status, &body)
        })
    }
}

pub(crate) fn validate_show_response(
    profile: &EvaluationProfile,
    status: StatusCode,
    body: &Value,
) -> Result<ModelIdentity> {
    if !status.is_success() {
        let detail = body
            .pointer("/error")
            .and_then(Value::as_str)
            .unwrap_or("модель не найдена");
        bail!(
            "профиль {} ({}) недоступен в Ollama: {}",
            profile.id,
            profile.model,
            safe_diagnostic(detail)
        );
    }
    let family = body
        .pointer("/details/family")
        .and_then(Value::as_str)
        .context("Ollama /api/show не вернула family")?;
    let quantization = body
        .pointer("/details/quantization_level")
        .and_then(Value::as_str)
        .context("Ollama /api/show не вернула quantization_level")?;
    let parameter_size = body
        .pointer("/details/parameter_size")
        .and_then(Value::as_str)
        .context("Ollama /api/show не вернула parameter_size")?;
    let parameters = body
        .get("parameters")
        .and_then(Value::as_str)
        .context("Ollama /api/show не вернула параметры alias")?;
    anyhow::ensure!(
        family == EXPECTED_FAMILY,
        "профиль {} использует несовместимое семейство {family}",
        profile.id
    );
    anyhow::ensure!(
        quantization == profile.expected_quantization,
        "профиль {} использует quantization {quantization}, ожидалось {}",
        profile.id,
        profile.expected_quantization
    );
    anyhow::ensure!(
        parameter_size == EXPECTED_PARAMETER_SIZE,
        "профиль {} использует размер {parameter_size}, ожидалось {EXPECTED_PARAMETER_SIZE}",
        profile.id
    );
    anyhow::ensure!(
        parameter_line_matches(parameters, "num_ctx", profile.num_ctx),
        "профиль {} не подтверждает num_ctx {}",
        profile.id,
        profile.num_ctx
    );
    anyhow::ensure!(
        parameter_line_matches(parameters, "num_predict", profile.num_predict),
        "профиль {} не подтверждает num_predict {}",
        profile.id,
        profile.num_predict
    );
    Ok(ModelIdentity {
        alias: profile.model.clone(),
        family: family.to_owned(),
        parameter_size: Some(parameter_size.to_owned()),
        quantization: quantization.to_owned(),
    })
}

fn parameter_line_matches(parameters: &str, name: &str, expected: usize) -> bool {
    parameters.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some(name)
            && fields.next().and_then(|value| value.parse().ok()) == Some(expected)
            && fields.next().is_none()
    })
}

pub(crate) fn parse_ps_response(
    model: &str,
    status: StatusCode,
    body: &Value,
) -> Result<RuntimeMetadata> {
    anyhow::ensure!(status.is_success(), "Ollama /api/ps вернула {status}");
    let models = body
        .get("models")
        .and_then(Value::as_array)
        .context("Ollama /api/ps не вернула список models")?;
    let wanted = canonical_model_name(model);
    let entry = models
        .iter()
        .find(|entry| {
            ["model", "name"]
                .into_iter()
                .filter_map(|field| entry.get(field).and_then(Value::as_str))
                .any(|name| canonical_model_name(name) == wanted)
        })
        .with_context(|| format!("профиль {model} не найден среди загруженных моделей"))?;
    let context_length = entry.get("context_length").and_then(Value::as_u64);
    let model_size_bytes = entry.get("size").and_then(Value::as_u64);
    let loaded_memory_bytes = entry.get("size_vram").and_then(Value::as_u64);
    let quantization = entry
        .pointer("/details/quantization_level")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut unavailable = Vec::new();
    if context_length.is_none() {
        unavailable.push("context_length не предоставлен Ollama".to_owned());
    }
    if model_size_bytes.is_none() {
        unavailable.push("size не предоставлен Ollama".to_owned());
    }
    if loaded_memory_bytes.is_none() {
        unavailable.push("size_vram не предоставлен Ollama".to_owned());
    }
    if quantization.is_none() {
        unavailable.push("quantization_level не предоставлен Ollama".to_owned());
    }
    Ok(RuntimeMetadata {
        context_length,
        model_size_bytes,
        loaded_memory_bytes,
        quantization,
        unavailable,
    })
}

fn canonical_model_name(value: &str) -> &str {
    value.strip_suffix(":latest").unwrap_or(value)
}

pub(crate) fn prompt_for(kind: PromptKind) -> &'static str {
    match kind {
        PromptKind::General => {
            "Кратко суммаризируй протокол встречи. Верни только один JSON-объект без Markdown с полями summary, decisions, action_items, open_questions и risks. Для элементов сохрани указанные в протоколе ID. У action_items должны быть id, task, owner и deadline; неизвестные owner и deadline укажи как null."
        }
        PromptKind::Specialized => {
            "Ты анализируешь протокол рабочей встречи. Верни только один валидный JSON-объект без Markdown, пояснений и текста до или после него. Обязательные поля: summary (строка, не более пяти коротких предложений), decisions (массив объектов id/text), action_items (массив объектов id/task/owner/deadline), open_questions (массив объектов id/text), risks (массив объектов id/text). Используй только факты протокола и сохраняй ID сущностей. Не придумывай решения, сроки и ответственных: отсутствующие owner или deadline должны быть null. Более позднее явно подтверждённое решение заменяет раннее; предложение не является решением без подтверждения; отменённые решения не включай в decisions; нерешённые вопросы помести в open_questions."
        }
    }
}

pub(crate) fn parse_summary(value: &str) -> Result<MeetingSummary> {
    let trimmed = value.trim();
    anyhow::ensure!(
        trimmed.starts_with('{') && trimmed.ends_with('}'),
        "ответ должен содержать только один JSON-объект"
    );
    anyhow::ensure!(
        !trimmed.contains("```") && !trimmed[..trimmed.len() - 1].contains("}\n{"),
        "ответ содержит Markdown или несколько JSON-объектов"
    );
    let summary: MeetingSummary =
        serde_json::from_str(trimmed).context("ответ не соответствует JSON-контракту")?;
    validate_summary(&summary, "ответ", false)?;
    Ok(summary)
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub(crate) struct QualityChecks {
    pub(crate) json_valid: bool,
    pub(crate) decisions_ok: bool,
    pub(crate) action_items_ok: bool,
    pub(crate) owners_ok: bool,
    pub(crate) deadlines_ok: bool,
    pub(crate) open_questions_ok: bool,
    pub(crate) risks_ok: bool,
    pub(crate) superseded_facts_absent: bool,
    pub(crate) unsupported_facts_absent: bool,
    pub(crate) summary_length_ok: bool,
}

impl QualityChecks {
    pub(crate) fn passed(&self) -> bool {
        self.json_valid
            && self.decisions_ok
            && self.action_items_ok
            && self.owners_ok
            && self.deadlines_ok
            && self.open_questions_ok
            && self.risks_ok
            && self.superseded_facts_absent
            && self.unsupported_facts_absent
            && self.summary_length_ok
    }
}

pub(crate) fn assess_summary(
    actual: &MeetingSummary,
    expected: &MeetingSummary,
    forbidden_fragments: &[String],
    raw_answer: &str,
) -> QualityChecks {
    let decisions_ok = facts_equal(&actual.decisions, &expected.decisions);
    let open_questions_ok = facts_equal(&actual.open_questions, &expected.open_questions);
    let risks_ok = facts_equal(&actual.risks, &expected.risks);
    let action_items_ok = same_ids_and_text(&actual.action_items, &expected.action_items, |item| {
        (&item.id, &item.task)
    });
    let owners_ok = values_by_id_equal(&actual.action_items, &expected.action_items, |item| {
        item.owner.as_deref()
    });
    let deadlines_ok = values_by_id_equal(&actual.action_items, &expected.action_items, |item| {
        item.deadline.as_deref()
    });
    let unsupported_facts_absent = facts_have_no_extra_ids(&actual.decisions, &expected.decisions)
        && facts_have_no_extra_ids(&actual.open_questions, &expected.open_questions)
        && facts_have_no_extra_ids(&actual.risks, &expected.risks)
        && actual.action_items.iter().all(|item| {
            expected
                .action_items
                .iter()
                .any(|expected| expected.id == item.id)
        });
    let answer = normalized(raw_answer);
    QualityChecks {
        json_valid: true,
        decisions_ok,
        action_items_ok,
        owners_ok,
        deadlines_ok,
        open_questions_ok,
        risks_ok,
        superseded_facts_absent: forbidden_fragments
            .iter()
            .all(|fragment| !answer.contains(&normalized(fragment))),
        unsupported_facts_absent,
        summary_length_ok: !actual.summary.trim().is_empty()
            && sentence_count(&actual.summary) <= 5,
    }
}

fn facts_equal(actual: &[SummaryFact], expected: &[SummaryFact]) -> bool {
    actual.len() == expected.len()
        && expected.iter().all(|fact| {
            actual
                .iter()
                .any(|item| item.id == fact.id && normalized(&item.text) == normalized(&fact.text))
        })
}

fn facts_have_no_extra_ids(actual: &[SummaryFact], expected: &[SummaryFact]) -> bool {
    actual
        .iter()
        .all(|fact| expected.iter().any(|item| item.id == fact.id))
}

fn same_ids_and_text<T, F>(actual: &[T], expected: &[T], key: F) -> bool
where
    F: Fn(&T) -> (&String, &String),
{
    actual.len() == expected.len()
        && expected.iter().all(|expected_item| {
            let (expected_id, expected_text) = key(expected_item);
            actual.iter().any(|actual_item| {
                let (actual_id, actual_text) = key(actual_item);
                actual_id == expected_id && normalized(actual_text) == normalized(expected_text)
            })
        })
}

fn values_by_id_equal<'a, F>(actual: &'a [ActionItem], expected: &'a [ActionItem], value: F) -> bool
where
    F: Fn(&'a ActionItem) -> Option<&'a str>,
{
    expected.iter().all(|expected_item| {
        actual
            .iter()
            .find(|item| item.id == expected_item.id)
            .is_some_and(|actual_item| {
                value(actual_item).map(normalized) == value(expected_item).map(normalized)
            })
    })
}

fn normalized(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn sentence_count(value: &str) -> usize {
    let count = value
        .split(['.', '!', '?'])
        .filter(|part| !part.trim().is_empty())
        .count();
    count.max(usize::from(!value.trim().is_empty()))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct PerformanceMetrics {
    pub(crate) elapsed_ms: u128,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) output_tokens_per_second: Option<f64>,
    pub(crate) throughput_unavailable_reason: Option<String>,
}

pub(crate) fn performance_metrics(
    elapsed_ms: u128,
    input_tokens: u64,
    output_tokens: u64,
) -> PerformanceMetrics {
    let output_tokens_per_second = (elapsed_ms > 0 && output_tokens > 0)
        .then_some(output_tokens as f64 * 1000.0 / elapsed_ms as f64);
    PerformanceMetrics {
        elapsed_ms,
        input_tokens: (input_tokens > 0).then_some(input_tokens),
        output_tokens: (output_tokens > 0).then_some(output_tokens),
        output_tokens_per_second,
        throughput_unavailable_reason: output_tokens_per_second.is_none().then(|| {
            if elapsed_ms == 0 {
                "elapsed_ms равен нулю".to_owned()
            } else {
                "output_tokens не предоставлены Ollama".to_owned()
            }
        }),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct CaseReport {
    pub(crate) scenario_id: String,
    pub(crate) length_tier: LengthTier,
    pub(crate) profile_id: String,
    pub(crate) cold: bool,
    pub(crate) answer: Option<String>,
    pub(crate) checks: QualityChecks,
    pub(crate) performance: PerformanceMetrics,
    pub(crate) runtime: RuntimeMetadata,
    pub(crate) error: Option<String>,
    pub(crate) passed: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct AggregateReport {
    pub(crate) profile_id: String,
    pub(crate) length_tier: Option<LengthTier>,
    pub(crate) total: usize,
    pub(crate) passed: usize,
    pub(crate) pass_rate: f64,
    pub(crate) cold_runs: usize,
    pub(crate) warm_runs: usize,
    pub(crate) median_warm_elapsed_ms: Option<u128>,
    pub(crate) median_loaded_memory_bytes: Option<u64>,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct SummarizationReport {
    pub(crate) generated_at_unix_ms: u128,
    pub(crate) base_model: String,
    pub(crate) quantization: String,
    pub(crate) profiles: Vec<EvaluationProfile>,
    pub(crate) cases: Vec<CaseReport>,
    pub(crate) aggregates: Vec<AggregateReport>,
    pub(crate) recommended_profile: Option<String>,
}

pub(crate) fn aggregate_reports(
    reports: &[CaseReport],
    profiles: &[EvaluationProfile],
) -> Vec<AggregateReport> {
    let mut aggregates = Vec::new();
    for profile in profiles {
        for tier in [
            None,
            Some(LengthTier::Short),
            Some(LengthTier::Medium),
            Some(LengthTier::Long),
        ] {
            let selected = reports
                .iter()
                .filter(|report| {
                    report.profile_id == profile.id
                        && tier.is_none_or(|value| report.length_tier == value)
                })
                .collect::<Vec<_>>();
            if selected.is_empty() {
                continue;
            }
            let mut warm_times = selected
                .iter()
                .filter(|report| !report.cold)
                .map(|report| report.performance.elapsed_ms)
                .collect::<Vec<_>>();
            let mut memory = selected
                .iter()
                .filter_map(|report| report.runtime.loaded_memory_bytes)
                .collect::<Vec<_>>();
            let passed = selected.iter().filter(|report| report.passed).count();
            aggregates.push(AggregateReport {
                profile_id: profile.id.clone(),
                length_tier: tier,
                total: selected.len(),
                passed,
                pass_rate: passed as f64 / selected.len() as f64,
                cold_runs: selected.iter().filter(|report| report.cold).count(),
                warm_runs: selected.iter().filter(|report| !report.cold).count(),
                median_warm_elapsed_ms: median(&mut warm_times),
                median_loaded_memory_bytes: median(&mut memory),
                input_tokens: selected
                    .iter()
                    .filter_map(|report| report.performance.input_tokens)
                    .sum(),
                output_tokens: selected
                    .iter()
                    .filter_map(|report| report.performance.output_tokens)
                    .sum(),
            });
        }
    }
    aggregates
}

fn median<T: Ord + Copy>(values: &mut [T]) -> Option<T> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

pub(crate) fn recommend_profile(aggregates: &[AggregateReport]) -> Option<String> {
    let mut candidates = aggregates
        .iter()
        .filter(|aggregate| aggregate.length_tier.is_none() && aggregate.profile_id != "baseline")
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .pass_rate
            .total_cmp(&left.pass_rate)
            .then_with(|| {
                compare_options(left.median_warm_elapsed_ms, right.median_warm_elapsed_ms)
            })
            .then_with(|| {
                compare_options(
                    left.median_loaded_memory_bytes,
                    right.median_loaded_memory_bytes,
                )
            })
            .then_with(|| left.profile_id.cmp(&right.profile_id))
    });
    candidates.first().map(|value| value.profile_id.clone())
}

fn compare_options<T: Ord>(left: Option<T>, right: Option<T>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub(crate) async fn run_summarization_evaluation(options: SummarizeEvalOptions) -> Result<()> {
    let root = std::env::current_dir()?.canonicalize()?;
    let dataset = load_dataset(&root, &options.dataset)?;
    let profiles = load_profiles(&root, &options.profiles)?;
    let client = Client::builder().user_agent("fox-llm/0.1.0").build()?;
    let inspector = Arc::new(LiveOllamaInspector::new(client.clone()));
    let mut progress = std::io::stderr().lock();
    let report = run_evaluation_with_clients(
        dataset,
        profiles,
        client,
        Arc::new(LiveRequestClient),
        inspector,
        &mut progress,
    )
    .await?;
    let output = resolve(&root, &options.output);
    write_json_atomically(&output, &report)?;
    progress_log(
        &mut progress,
        format_args!("[summarize-eval][report_written] {}", output.display()),
    )?;
    Ok(())
}

pub(crate) async fn run_evaluation_with_clients(
    dataset: BenchmarkDataset,
    profiles: ProfileSet,
    client: Client,
    request_client: Arc<dyn RequestClient>,
    inspector: Arc<dyn OllamaInspector>,
    progress: &mut dyn Write,
) -> Result<SummarizationReport> {
    validate_dataset(&dataset)?;
    validate_profiles(&profiles)?;
    let total = dataset.scenarios.len() * profiles.profiles.len();
    progress_log(
        progress,
        format_args!(
            "[summarize-eval][start] scenarios={} profiles={} requests={total}",
            dataset.scenarios.len(),
            profiles.profiles.len()
        ),
    )?;
    for profile in &profiles.profiles {
        progress_log(
            progress,
            format_args!(
                "[summarize-eval][preflight] profile={} model={}",
                safe_id(&profile.id),
                safe_id(&profile.model)
            ),
        )?;
        let identity = match inspector.preflight(profile).await {
            Ok(identity) => identity,
            Err(error) => {
                let detail = safe_diagnostic(&error.to_string());
                progress_log(
                    progress,
                    format_args!(
                        "[summarize-eval][preflight_failed] profile={} error={detail}",
                        safe_id(&profile.id)
                    ),
                )?;
                bail!("preflight профиля {}: {detail}", safe_id(&profile.id));
            }
        };
        anyhow::ensure!(
            identity.quantization == EXPECTED_QUANTIZATION,
            "профиль {} не прошёл проверку quantization",
            profile.id
        );
    }

    let mut cases = Vec::with_capacity(total);
    let mut warmed = HashSet::new();
    let mut ordinal = 0;
    for (scenario_index, scenario) in dataset.scenarios.iter().enumerate() {
        for offset in 0..profiles.profiles.len() {
            let profile = &profiles.profiles[(scenario_index + offset) % profiles.profiles.len()];
            ordinal += 1;
            let cold = warmed.insert(profile.id.clone());
            progress_log(
                progress,
                format_args!(
                    "[summarize-eval][case_start] {ordinal}/{total} scenario={} profile={} state={}",
                    safe_id(&scenario.id),
                    safe_id(&profile.id),
                    if cold { "cold" } else { "warm" }
                ),
            )?;
            let report = run_case(
                scenario,
                profile,
                cold,
                &client,
                request_client.as_ref(),
                inspector.as_ref(),
            )
            .await;
            progress_log(
                progress,
                format_args!(
                    "[summarize-eval][case_done] {ordinal}/{total} scenario={} profile={} passed={} elapsed_ms={} input_tokens={} output_tokens={} error={}",
                    safe_id(&scenario.id),
                    safe_id(&profile.id),
                    report.passed,
                    report.performance.elapsed_ms,
                    report.performance.input_tokens.unwrap_or(0),
                    report.performance.output_tokens.unwrap_or(0),
                    report.error.as_deref().map(safe_diagnostic).unwrap_or_else(|| "none".to_owned())
                ),
            )?;
            cases.push(report);
        }
    }
    let aggregates = aggregate_reports(&cases, &profiles.profiles);
    let recommended_profile = recommend_profile(&aggregates);
    progress_log(
        progress,
        format_args!(
            "[summarize-eval][aggregate] cases={} passed={} recommended={}",
            cases.len(),
            cases.iter().filter(|case| case.passed).count(),
            recommended_profile.as_deref().unwrap_or("none")
        ),
    )?;
    Ok(SummarizationReport {
        generated_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        base_model: EXPECTED_BASE_MODEL.to_owned(),
        quantization: EXPECTED_QUANTIZATION.to_owned(),
        profiles: profiles.profiles,
        cases,
        aggregates,
        recommended_profile,
    })
}

async fn run_case(
    scenario: &MeetingScenario,
    profile: &EvaluationProfile,
    cold: bool,
    client: &Client,
    request_client: &dyn RequestClient,
    inspector: &dyn OllamaInspector,
) -> CaseReport {
    let settings = AgentSettings {
        provider: Provider::Ollama,
        api_key: None,
        endpoint: None,
        model: profile.model.clone(),
        temperature: profile.temperature,
        instructions: Some(prompt_for(profile.prompt).to_owned()),
        compression_strategy: CompressionStrategy::SlidingWindow,
        context_messages: 1,
    };
    let history = vec![Message {
        role: "user".to_owned(),
        content: format!("Протокол встречи:\n{}", scenario.rendered_transcript()),
    }];
    let started = Instant::now();
    let answer = request_client.send(client, &settings, &history).await;
    let elapsed_ms = started.elapsed().as_millis();
    match answer {
        Ok(answer) => successful_case(scenario, profile, cold, answer, elapsed_ms, inspector).await,
        Err(error) => failed_case(scenario, profile, cold, elapsed_ms, error),
    }
}

async fn successful_case(
    scenario: &MeetingScenario,
    profile: &EvaluationProfile,
    cold: bool,
    answer: ApiAnswer,
    elapsed_ms: u128,
    inspector: &dyn OllamaInspector,
) -> CaseReport {
    let performance = performance_metrics(elapsed_ms, answer.input_tokens, answer.output_tokens);
    let runtime = inspector
        .runtime(&profile.model)
        .await
        .unwrap_or_else(|error| RuntimeMetadata {
            unavailable: vec![safe_diagnostic(&error.to_string())],
            ..RuntimeMetadata::default()
        });
    match parse_summary(&answer.text) {
        Ok(parsed) => {
            let checks = assess_summary(
                &parsed,
                &scenario.expected,
                &scenario.forbidden_fragments,
                &answer.text,
            );
            let passed = checks.passed();
            CaseReport {
                scenario_id: scenario.id.clone(),
                length_tier: scenario.length_tier,
                profile_id: profile.id.clone(),
                cold,
                answer: Some(sanitize_answer(&answer.text)),
                checks,
                performance,
                runtime,
                error: None,
                passed,
            }
        }
        Err(error) => CaseReport {
            scenario_id: scenario.id.clone(),
            length_tier: scenario.length_tier,
            profile_id: profile.id.clone(),
            cold,
            answer: Some(sanitize_answer(&answer.text)),
            checks: QualityChecks::default(),
            performance,
            runtime,
            error: Some(safe_diagnostic(&error.to_string())),
            passed: false,
        },
    }
}

fn failed_case(
    scenario: &MeetingScenario,
    profile: &EvaluationProfile,
    cold: bool,
    elapsed_ms: u128,
    error: anyhow::Error,
) -> CaseReport {
    CaseReport {
        scenario_id: scenario.id.clone(),
        length_tier: scenario.length_tier,
        profile_id: profile.id.clone(),
        cold,
        answer: None,
        checks: QualityChecks::default(),
        performance: performance_metrics(elapsed_ms, 0, 0),
        runtime: RuntimeMetadata {
            unavailable: vec!["runtime metadata недоступна после ошибки generation".to_owned()],
            ..RuntimeMetadata::default()
        },
        error: Some(safe_diagnostic(&error.to_string())),
        passed: false,
    }
}

fn progress_log(output: &mut dyn Write, arguments: std::fmt::Arguments<'_>) -> Result<()> {
    writeln!(output, "{arguments}").context("не удалось записать progress-лог")
}

fn safe_id(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':') {
                character
            } else {
                '_'
            }
        })
        .take(80)
        .collect()
}

fn safe_diagnostic(value: &str) -> String {
    let lowered = value.to_lowercase();
    if ["authorization", "bearer ", "api_key", "api key"]
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return "подробности ошибки скрыты из-за потенциально чувствительных данных".to_owned();
    }
    value
        .chars()
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .take(300)
        .collect()
}

pub(crate) fn sanitize_answer(value: &str) -> String {
    let lowered = value.to_lowercase();
    if ["authorization", "bearer ", "api_key", "api key"]
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return "[ответ скрыт из-за потенциально чувствительных данных]".to_owned();
    }
    value
        .chars()
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .take(20_000)
        .collect()
}

pub(crate) fn write_json_atomically(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("не удалось создать {}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("некорректное имя output-файла")?;
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("не удалось создать {}", temporary.display()))?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
