use anyhow::Result;
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc};

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
