use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

pub(crate) const PIPELINE_TOOL_NAME: &str = "fox_execute_pipeline";
pub(crate) const MAX_PIPELINE_STEPS: usize = 8;
pub(crate) const DEFAULT_MCP_TOOL_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const GITHUB_ACTIVITY_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) fn mcp_tool_timeout(tool_name: &str, default: Duration) -> Duration {
    if tool_name == "github_project_activity" {
        default.max(GITHUB_ACTIVITY_TIMEOUT)
    } else {
        default
    }
}

/// Provider-neutral description of an MCP tool exposed to the model.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolDefinition {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) input_schema: Value,
    pub(crate) read_only: bool,
    pub(crate) destructive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolResult {
    pub(crate) call_id: String,
    pub(crate) content: Value,
    pub(crate) is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PipelinePlan {
    pub(crate) name: String,
    pub(crate) steps: Vec<PipelineStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PipelineStep {
    pub(crate) id: String,
    pub(crate) tool: String,
    pub(crate) arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PipelineStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PipelineStepStatus {
    Succeeded,
    Failed,
    Cancelled,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PipelineStepTrace {
    pub(crate) id: String,
    pub(crate) tool: String,
    pub(crate) status: PipelineStepStatus,
    pub(crate) arguments: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    pub(crate) state_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PipelineTrace {
    pub(crate) name: String,
    pub(crate) status: PipelineStatus,
    pub(crate) steps: Vec<PipelineStepTrace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

pub(crate) fn pipeline_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: PIPELINE_TOOL_NAME.to_owned(),
        description: Some(
            "Выполняет последовательный пайплайн из разрешённых MCP-инструментов; результаты предыдущих шагов доступны как {\"$ref\":\"step_id.output\"}"
                .to_owned(),
        ),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["name", "steps"],
            "properties": {
                "name": {"type": "string", "minLength": 1},
                "steps": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_PIPELINE_STEPS,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["id", "tool", "arguments"],
                        "properties": {
                            "id": {"type": "string", "minLength": 1},
                            "tool": {"type": "string", "minLength": 1},
                            "arguments": {"type": "object"}
                        }
                    }
                }
            }
        }),
        read_only: false,
        destructive: false,
    }
}

pub(crate) fn parse_pipeline_plan(arguments: &Value) -> Result<PipelinePlan> {
    serde_json::from_value(arguments.clone()).context("неверный формат MCP-пайплайна")
}

pub(crate) fn validate_pipeline_plan(
    plan: &PipelinePlan,
    definitions: &[ToolDefinition],
) -> Result<()> {
    if plan.name.trim().is_empty() {
        bail!("имя MCP-пайплайна не должно быть пустым");
    }
    if plan.steps.is_empty() || plan.steps.len() > MAX_PIPELINE_STEPS {
        bail!("MCP-пайплайн должен содержать от 1 до {MAX_PIPELINE_STEPS} шагов");
    }
    let definitions = definitions
        .iter()
        .map(|definition| (definition.name.as_str(), definition))
        .collect::<BTreeMap<_, _>>();
    let mut previous = BTreeSet::new();
    for step in &plan.steps {
        if step.id.trim().is_empty() {
            bail!("идентификатор шага MCP-пайплайна не должен быть пустым");
        }
        if previous.contains(step.id.as_str()) {
            bail!(
                "повторяющийся идентификатор шага MCP-пайплайна: {}",
                step.id
            );
        }
        if step.tool == PIPELINE_TOOL_NAME {
            bail!("MCP-пайплайн не может вызывать {PIPELINE_TOOL_NAME}");
        }
        if !definitions.contains_key(step.tool.as_str()) {
            bail!("MCP-инструмент «{}» не разрешён для пайплайна", step.tool);
        }
        if !step.arguments.is_object() {
            bail!("аргументы шага «{}» должны быть JSON-объектом", step.id);
        }
        validate_references(&step.arguments, &previous, &step.id)?;
        previous.insert(step.id.as_str());
    }
    Ok(())
}

fn validate_references(value: &Value, previous: &BTreeSet<&str>, step_id: &str) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                validate_references(item, previous, step_id)?;
            }
        }
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref") {
                if object.len() != 1 {
                    bail!("$ref шага «{step_id}» должен быть единственным полем объекта");
                }
                let reference = reference
                    .as_str()
                    .ok_or_else(|| anyhow!("$ref шага «{step_id}» должен быть строкой"))?;
                let source = reference.strip_suffix(".output").ok_or_else(|| {
                    anyhow!("$ref шага «{step_id}» должен иметь вид <step-id>.output")
                })?;
                if !previous.contains(source) {
                    bail!("$ref шага «{step_id}» указывает не на предыдущий шаг: {reference}");
                }
            } else {
                for item in object.values() {
                    validate_references(item, previous, step_id)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn resolve_pipeline_arguments(
    arguments: &Value,
    outputs: &BTreeMap<String, Value>,
) -> Result<Value> {
    match arguments {
        Value::Array(items) => items
            .iter()
            .map(|item| resolve_pipeline_arguments(item, outputs))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref") {
                if object.len() != 1 {
                    bail!("$ref должен быть единственным полем объекта");
                }
                let reference = reference
                    .as_str()
                    .ok_or_else(|| anyhow!("$ref должен быть строкой"))?;
                let source = reference
                    .strip_suffix(".output")
                    .ok_or_else(|| anyhow!("$ref должен иметь вид <step-id>.output"))?;
                outputs
                    .get(source)
                    .cloned()
                    .ok_or_else(|| anyhow!("результат шага «{source}» недоступен"))
            } else {
                object
                    .iter()
                    .map(|(key, value)| {
                        Ok((key.clone(), resolve_pipeline_arguments(value, outputs)?))
                    })
                    .collect::<Result<Map<String, Value>>>()
                    .map(Value::Object)
            }
        }
        _ => Ok(arguments.clone()),
    }
}

pub(crate) fn validate_tool_arguments(
    definition: &ToolDefinition,
    arguments: &Value,
) -> Result<()> {
    if !arguments.is_object() {
        bail!(
            "аргументы MCP-инструмента «{}» должны быть JSON-объектом",
            definition.name
        );
    }
    let validator = jsonschema::validator_for(&definition.input_schema).with_context(|| {
        format!(
            "MCP-инструмент «{}» объявил некорректную входную схему",
            definition.name
        )
    })?;
    if let Err(error) = validator.validate(arguments) {
        bail!(
            "аргументы MCP-инструмента «{}» не соответствуют схеме: {error}",
            definition.name
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RequestOptions {
    pub(crate) tools: Vec<ToolDefinition>,
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) tool_results: Vec<ToolResult>,
}

pub(crate) type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>>;

pub(crate) trait ToolExecutor: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;
    fn execute<'a>(&'a self, call: &'a ToolCall) -> ToolFuture<'a>;
}

pub(crate) trait ToolApproval: Send + Sync {
    fn approve(&self, definition: &ToolDefinition, call: &ToolCall) -> Result<bool>;
}

pub(crate) struct AutoApprove;

impl ToolApproval for AutoApprove {
    fn approve(&self, _definition: &ToolDefinition, _call: &ToolCall) -> Result<bool> {
        Ok(true)
    }
}

pub(crate) type SharedToolExecutor = Arc<dyn ToolExecutor>;
pub(crate) type SharedToolApproval = Arc<dyn ToolApproval>;
