use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};

use super::{
    mcp_tool_timeout, resolve_pipeline_arguments_with_runtime, validate_pipeline_plan,
    validate_tool_arguments, PipelinePlan, PipelineStatus, PipelineStepStatus, PipelineStepTrace,
    PipelineTrace, SharedToolExecutor, ToolCall, ToolDefinition, ToolOutcome, ToolResult,
};

pub(crate) type AuthorizationFuture<'a> = Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;

pub(crate) trait PipelineExecutionPolicy {
    fn authorize<'a>(
        &'a mut self,
        definition: &'a ToolDefinition,
        call: &'a ToolCall,
    ) -> AuthorizationFuture<'a>;
}

pub(crate) struct PreauthorizedPolicy;

impl PipelineExecutionPolicy for PreauthorizedPolicy {
    fn authorize<'a>(
        &'a mut self,
        _definition: &'a ToolDefinition,
        _call: &'a ToolCall,
    ) -> AuthorizationFuture<'a> {
        Box::pin(async { Ok(true) })
    }
}

fn identity(tool: &str) -> (Option<String>, String) {
    tool.split_once("__").map_or_else(
        || (None, tool.to_owned()),
        |(server, native)| (Some(server.to_owned()), native.to_owned()),
    )
}

#[allow(clippy::too_many_arguments)]
fn step_trace(
    ordinal: usize,
    id: &str,
    tool: &str,
    status: PipelineStepStatus,
    outcome: ToolOutcome,
    duration_ms: u128,
    arguments: Value,
    output: Option<Value>,
    error: Option<String>,
    state_changed: bool,
) -> PipelineStepTrace {
    let (server_id, native_tool) = identity(tool);
    PipelineStepTrace {
        ordinal,
        id: id.to_owned(),
        tool: tool.to_owned(),
        server_id,
        native_tool,
        status,
        outcome,
        duration_ms,
        arguments,
        output,
        error,
        state_changed,
    }
}

fn pipeline_result(
    call_id: &str,
    plan: &PipelinePlan,
    mut traces: Vec<PipelineStepTrace>,
    skipped_from: usize,
    status: PipelineStatus,
    error: Option<String>,
) -> ToolResult {
    traces.extend(
        plan.steps
            .iter()
            .skip(skipped_from)
            .enumerate()
            .map(|(offset, step)| {
                step_trace(
                    skipped_from + offset + 1,
                    &step.id,
                    &step.tool,
                    PipelineStepStatus::Skipped,
                    ToolOutcome::Failed,
                    0,
                    step.arguments.clone(),
                    None,
                    None,
                    false,
                )
            }),
    );
    let trace = PipelineTrace {
        name: plan.name.clone(),
        status,
        steps: traces,
        error,
    };
    let outcome = match status {
        PipelineStatus::Succeeded => ToolOutcome::Success,
        PipelineStatus::Unknown => ToolOutcome::Unknown,
        PipelineStatus::Failed | PipelineStatus::Cancelled => ToolOutcome::Failed,
    };
    ToolResult {
        call_id: call_id.to_owned(),
        content: serde_json::to_value(trace)
            .unwrap_or_else(|error| json!({"status":"failed","error":error.to_string()})),
        is_error: outcome != ToolOutcome::Success,
        outcome,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_pipeline<P: PipelineExecutionPolicy>(
    call_id: &str,
    plan: &PipelinePlan,
    definitions: &[ToolDefinition],
    executor: SharedToolExecutor,
    runtime: &Value,
    policy: &mut P,
    default_timeout: Duration,
) -> ToolResult {
    if let Err(error) = validate_pipeline_plan(plan, definitions) {
        return pipeline_result(
            call_id,
            plan,
            Vec::new(),
            0,
            PipelineStatus::Failed,
            Some(error.to_string()),
        );
    }
    let mut outputs = BTreeMap::new();
    let mut traces = Vec::with_capacity(plan.steps.len());
    for (index, step) in plan.steps.iter().enumerate() {
        let Some(definition) = definitions
            .iter()
            .find(|definition| definition.name == step.tool)
        else {
            return pipeline_result(
                call_id,
                plan,
                traces,
                index,
                PipelineStatus::Failed,
                Some(format!("MCP-инструмент «{}» не разрешён", step.tool)),
            );
        };
        let arguments =
            match resolve_pipeline_arguments_with_runtime(&step.arguments, &outputs, runtime) {
                Ok(arguments) => arguments,
                Err(error) => {
                    traces.push(step_trace(
                        index + 1,
                        &step.id,
                        &step.tool,
                        PipelineStepStatus::Failed,
                        ToolOutcome::Failed,
                        0,
                        step.arguments.clone(),
                        None,
                        Some(error.to_string()),
                        false,
                    ));
                    return pipeline_result(
                        call_id,
                        plan,
                        traces,
                        index + 1,
                        PipelineStatus::Failed,
                        Some(format!("не удалось подготовить шаг «{}»", step.id)),
                    );
                }
            };
        if let Err(error) = validate_tool_arguments(definition, &arguments) {
            traces.push(step_trace(
                index + 1,
                &step.id,
                &step.tool,
                PipelineStepStatus::Failed,
                ToolOutcome::Failed,
                0,
                arguments,
                None,
                Some(error.to_string()),
                false,
            ));
            return pipeline_result(
                call_id,
                plan,
                traces,
                index + 1,
                PipelineStatus::Failed,
                Some(format!("аргументы шага «{}» не прошли проверку", step.id)),
            );
        }
        let step_call = ToolCall {
            id: format!("{call_id}:{}", step.id),
            name: step.tool.clone(),
            arguments: arguments.clone(),
        };
        match policy.authorize(definition, &step_call).await {
            Ok(true) => {}
            Ok(false) => {
                traces.push(step_trace(
                    index + 1,
                    &step.id,
                    &step.tool,
                    PipelineStepStatus::Cancelled,
                    ToolOutcome::Failed,
                    0,
                    arguments,
                    None,
                    Some("действие отменено пользователем".into()),
                    false,
                ));
                return pipeline_result(
                    call_id,
                    plan,
                    traces,
                    index + 1,
                    PipelineStatus::Cancelled,
                    Some("пайплайн отменён пользователем".into()),
                );
            }
            Err(error) => {
                traces.push(step_trace(
                    index + 1,
                    &step.id,
                    &step.tool,
                    PipelineStepStatus::Failed,
                    ToolOutcome::Failed,
                    0,
                    arguments,
                    None,
                    Some(error.to_string()),
                    false,
                ));
                return pipeline_result(
                    call_id,
                    plan,
                    traces,
                    index + 1,
                    PipelineStatus::Failed,
                    Some(format!("проверка шага «{}» завершилась ошибкой", step.id)),
                );
            }
        }
        let timeout = mcp_tool_timeout(&step_call.name, default_timeout);
        let started = Instant::now();
        let result = match tokio::time::timeout(timeout, executor.execute(&step_call)).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => ToolResult {
                call_id: step_call.id.clone(),
                content: json!({"error":error.to_string()}),
                is_error: true,
                outcome: ToolOutcome::Failed,
            },
            Err(_) => ToolResult {
                call_id: step_call.id.clone(),
                content: json!({"error":format!("истекло время выполнения MCP-инструмента ({} с)", timeout.as_secs_f64())}),
                is_error: true,
                outcome: ToolOutcome::Failed,
            },
        };
        let elapsed = started.elapsed().as_millis();
        if result.outcome != ToolOutcome::Success || result.is_error {
            let (pipeline_status, step_status) = if result.outcome == ToolOutcome::Unknown {
                (PipelineStatus::Unknown, PipelineStepStatus::Unknown)
            } else {
                (PipelineStatus::Failed, PipelineStepStatus::Failed)
            };
            traces.push(step_trace(
                index + 1,
                &step.id,
                &step.tool,
                step_status,
                result.outcome,
                elapsed,
                arguments,
                Some(result.content),
                Some("MCP-инструмент не подтвердил успех".into()),
                false,
            ));
            return pipeline_result(
                call_id,
                plan,
                traces,
                index + 1,
                pipeline_status,
                Some(format!(
                    "шаг «{}» завершился без подтверждённого успеха",
                    step.id
                )),
            );
        }
        let state_changed = !definition.read_only;
        outputs.insert(step.id.clone(), result.content.clone());
        traces.push(step_trace(
            index + 1,
            &step.id,
            &step.tool,
            PipelineStepStatus::Succeeded,
            result.outcome,
            elapsed,
            arguments,
            Some(result.content),
            None,
            state_changed,
        ));
    }
    pipeline_result(
        call_id,
        plan,
        traces,
        plan.steps.len(),
        PipelineStatus::Succeeded,
        None,
    )
}

pub(crate) fn parse_plan(arguments: &Value) -> Result<PipelinePlan> {
    serde_json::from_value(arguments.clone())
        .map_err(|error| anyhow!(error))
        .context("неверный формат MCP-пайплайна")
}
