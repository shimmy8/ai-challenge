#![allow(unused_imports)]

use anyhow::{anyhow, bail, Context, Result};
use console::style;
use dialoguer::{theme::ColorfulTheme, Input, MultiSelect, Select};
use rmcp::{
    model::{CallToolRequestParams, RequestParamsMeta, Tool},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::SocketAddr,
    path::Path,
    str::FromStr,
    sync::Arc,
    time::Instant,
};

mod calendar;
mod github;
mod group_servers;
mod orchestrator;
mod pipeline;
mod report;
mod scheduler;
mod server;
mod telegram;
mod tools;
pub(crate) use calendar::load_mcp_env_file;
pub(crate) use calendar::{
    CalDavClient, CalendarDigestEvent, CalendarEventRequest, CalendarEventResult,
};
pub(crate) use github::*;
pub(crate) use group_servers::*;
pub(crate) use orchestrator::*;
pub(crate) use pipeline::*;
pub(crate) use report::*;
pub(crate) use scheduler::*;
pub(crate) use server::run_mcp_server;
pub(crate) use telegram::*;
pub(crate) use tools::*;

pub(crate) const MCP_SERVER_URL: &str = "http://127.0.0.1:8000/mcp";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpServerKind {
    Github,
    Reporting,
    Workspace,
    Calendar,
    Ai,
    Telegram,
    Orchestrator,
}

impl McpServerKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Reporting => "reporting",
            Self::Workspace => "workspace",
            Self::Calendar => "calendar",
            Self::Ai => "ai",
            Self::Telegram => "telegram",
            Self::Orchestrator => "orchestrator",
        }
    }
}

impl fmt::Display for McpServerKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for McpServerKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "github" => Ok(Self::Github),
            "reporting" => Ok(Self::Reporting),
            "workspace" => Ok(Self::Workspace),
            "calendar" => Ok(Self::Calendar),
            "ai" => Ok(Self::Ai),
            "telegram" => Ok(Self::Telegram),
            "orchestrator" => Ok(Self::Orchestrator),
            _ => bail!("неизвестный MCP-сервер: {value}"),
        }
    }
}

pub(crate) fn validate_mcp_bind_addr(value: &str) -> Result<SocketAddr> {
    let address: SocketAddr = value.parse().context("некорректный адрес MCP-сервера")?;
    anyhow::ensure!(
        address.ip().is_loopback(),
        "MCP-сервер должен слушать loopback-адрес"
    );
    Ok(address)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpToolInfo {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) input_schema: serde_json::Value,
    pub(crate) read_only: bool,
    pub(crate) destructive: bool,
}

pub(crate) fn validate_mcp_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value.trim()).context("адрес MCP некорректен")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "адрес MCP должен начинаться с http:// или https://"
    );
    anyhow::ensure!(url.host_str().is_some(), "в адресе MCP не указан хост");
    Ok(url.to_string())
}

pub(crate) async fn fetch_tools(url: &str) -> Result<Vec<McpToolInfo>> {
    let url = validate_mcp_url(url)?;
    tokio::time::timeout(std::time::Duration::from_secs(5), fetch_tools_inner(&url))
        .await
        .map_err(|_| anyhow!("истекло время ожидания MCP-сервера"))?
}

async fn fetch_tools_inner(url: &str) -> Result<Vec<McpToolInfo>> {
    let session = McpSession::connect(url).await?;
    let tools = session.tools.clone();
    session.close().await;
    Ok(tools)
}

pub(crate) struct McpSession {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    pub(crate) tools: Vec<McpToolInfo>,
}

pub(crate) struct McpRouter {
    sessions: BTreeMap<String, McpSession>,
    routes: BTreeMap<String, ToolRoute>,
    warnings: Vec<String>,
    logger: Arc<dyn McpCallLogger>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct McpCallEvent<'a> {
    pub(crate) event: &'a str,
    pub(crate) correlation_id: &'a str,
    pub(crate) server_id: &'a str,
    pub(crate) tool: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) step_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) outcome: Option<ToolOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duration_ms: Option<u128>,
}

pub(crate) trait McpCallLogger: Send + Sync {
    fn log(&self, event: &McpCallEvent<'_>);
}

#[derive(Debug, Default)]
struct SilentMcpCallLogger;

impl McpCallLogger for SilentMcpCallLogger {
    fn log(&self, _event: &McpCallEvent<'_>) {}
}

#[derive(Debug, Default)]
pub(crate) struct StdoutMcpCallLogger;

impl McpCallLogger for StdoutMcpCallLogger {
    fn log(&self, event: &McpCallEvent<'_>) {
        if let Ok(line) = serde_json::to_string(event) {
            println!("{line}");
        }
    }
}

impl McpRouter {
    pub(crate) async fn connect(servers: &[crate::config::McpServerConfig]) -> Self {
        Self::connect_with_logger(servers, Arc::new(SilentMcpCallLogger)).await
    }

    pub(crate) async fn connect_with_logger(
        servers: &[crate::config::McpServerConfig],
        logger: Arc<dyn McpCallLogger>,
    ) -> Self {
        let mut sessions = BTreeMap::new();
        let mut routes = BTreeMap::new();
        let mut warnings = Vec::new();
        for server in servers {
            match McpSession::connect(&server.url).await {
                Ok(session) => {
                    for tool in enabled_tool_definitions(&session.tools, &server.disabled_tools) {
                        let public_name = qualify_tool_name(&server.id, &tool.name);
                        if routes.contains_key(&public_name) {
                            warnings.push(format!("повторяющийся маршрут MCP: {public_name}"));
                            continue;
                        }
                        let mut definition = tool;
                        let native_name = definition.name.clone();
                        definition.name = public_name.clone();
                        routes.insert(
                            public_name,
                            ToolRoute {
                                server_id: server.id.clone(),
                                native_name,
                                definition,
                            },
                        );
                    }
                    sessions.insert(server.id.clone(), session);
                }
                Err(error) => warnings.push(format!("{}: {error:#}", server.id)),
            }
        }
        Self {
            sessions,
            routes,
            warnings,
            logger,
        }
    }

    pub(crate) fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn route(&self, public_name: &str) -> Option<&ToolRoute> {
        self.routes.get(public_name)
    }
}

impl ToolExecutor for McpRouter {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.routes
            .values()
            .map(|route| route.definition.clone())
            .collect()
    }

    fn execute<'a>(&'a self, call: &'a ToolCall) -> ToolFuture<'a> {
        Box::pin(async move {
            let route = self
                .routes
                .get(&call.name)
                .ok_or_else(|| anyhow!("MCP-инструмент «{}» не разрешён", call.name))?;
            let session = self
                .sessions
                .get(&route.server_id)
                .ok_or_else(|| anyhow!("MCP-сервер «{}» недоступен", route.server_id))?;
            let native_call = ToolCall {
                id: call.id.clone(),
                name: route.native_name.clone(),
                arguments: call.arguments.clone(),
            };
            let correlation_id = mcp_correlation_id(&call.id);
            self.logger.log(&McpCallEvent {
                event: "mcp_call_start",
                correlation_id: &correlation_id,
                server_id: &route.server_id,
                tool: &route.native_name,
                step_id: call.id.rsplit_once(':').map(|(_, step)| step),
                outcome: None,
                duration_ms: None,
            });
            let started = Instant::now();
            let result = session.call(&native_call).await;
            self.logger.log(&McpCallEvent {
                event: "mcp_call_finish",
                correlation_id: &correlation_id,
                server_id: &route.server_id,
                tool: &route.native_name,
                step_id: call.id.rsplit_once(':').map(|(_, step)| step),
                outcome: Some(
                    result
                        .as_ref()
                        .map(|result| result.outcome)
                        .unwrap_or(ToolOutcome::Failed),
                ),
                duration_ms: Some(started.elapsed().as_millis()),
            });
            result
        })
    }
}

pub(crate) fn qualify_tool_name(server_id: &str, tool_name: &str) -> String {
    format!("{server_id}__{tool_name}")
}

fn mcp_correlation_id(value: &str) -> String {
    fn fnv(input: &[u8], seed: u64) -> u64 {
        input.iter().fold(seed, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
    }
    let first = fnv(value.as_bytes(), 0xcbf29ce484222325);
    let second = fnv(value.as_bytes(), 0x84222325cbf29ce4);
    let parent = fnv(value.as_bytes(), 0x9e3779b97f4a7c15);
    format!("00-{first:016x}{second:016x}-{parent:016x}-01")
}

pub(crate) fn log_server_started(kind: McpServerKind, address: SocketAddr) {
    println!(
        "{}",
        serde_json::json!({
            "event": "mcp_server_started",
            "server_id": kind.as_str(),
            "address": address.to_string(),
        })
    );
}

pub(crate) fn log_local_call(
    event: &str,
    correlation_id: &str,
    server_id: &str,
    tool: &str,
    step_id: Option<&str>,
    outcome: Option<ToolOutcome>,
    duration_ms: Option<u128>,
) {
    StdoutMcpCallLogger.log(&McpCallEvent {
        event,
        correlation_id,
        server_id,
        tool,
        step_id,
        outcome,
        duration_ms,
    });
}

impl McpSession {
    pub(crate) async fn connect(url: &str) -> Result<Self> {
        let url = validate_mcp_url(url)?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async move {
            let transport = StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(url),
            );
            let client = ().serve(transport).await.context("MCP handshake не выполнен")?;
            let result = client
                .list_all_tools()
                .await
                .context("MCP-сервер не вернул список инструментов")?;
            let tools = result
                .into_iter()
                .map(|tool| McpToolInfo {
                    name: tool.name.to_string(),
                    description: tool.description.map(|value| value.to_string()),
                    input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
                    read_only: tool
                        .annotations
                        .as_ref()
                        .and_then(|annotations| annotations.read_only_hint)
                        .unwrap_or(false),
                    destructive: tool
                        .annotations
                        .as_ref()
                        .and_then(|annotations| annotations.destructive_hint)
                        .unwrap_or(false),
                })
                .collect();
            Ok(Self { client, tools })
        })
        .await
        .map_err(|_| anyhow!("истекло время ожидания MCP-сервера"))?
    }

    pub(crate) async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let arguments = call
            .arguments
            .as_object()
            .cloned()
            .ok_or_else(|| anyhow!("аргументы MCP-инструмента должны быть JSON-объектом"))?;
        let mut request = CallToolRequestParams::new(call.name.clone()).with_arguments(arguments);
        request.set_traceparent(&mcp_correlation_id(&call.id));
        if let Some((_, step)) = call.id.rsplit_once(':') {
            let safe_step = step
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                        character
                    } else {
                        '_'
                    }
                })
                .collect::<String>();
            request.set_baggage(&format!("fox-step={safe_step}"));
        }
        let result = tokio::time::timeout(
            mcp_tool_timeout(&call.name, DEFAULT_MCP_TOOL_TIMEOUT),
            self.client.call_tool(request),
        )
        .await
        .map_err(|_| anyhow!("истекло время ожидания MCP-инструмента {}", call.name))?
        .with_context(|| format!("MCP-инструмент {} завершился ошибкой", call.name))?;
        let content = result
            .structured_content
            .unwrap_or_else(|| serde_json::to_value(&result.content).unwrap_or_default());
        let is_error = result.is_error.unwrap_or(false);
        let declared_outcome = content.get("outcome").and_then(serde_json::Value::as_str);
        let outcome = if is_error || declared_outcome == Some("failed") {
            ToolOutcome::Failed
        } else if declared_outcome == Some("unknown") {
            ToolOutcome::Unknown
        } else {
            ToolOutcome::Success
        };
        Ok(ToolResult {
            call_id: call.id.clone(),
            content,
            is_error: is_error || outcome != ToolOutcome::Success,
            outcome,
        })
    }

    pub(crate) async fn close(self) {
        let _ = self.client.cancel().await;
    }
}

pub(crate) fn reconcile_disabled_tools(
    available: &[McpToolInfo],
    disabled: &[String],
) -> Vec<String> {
    let available = available
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut result = disabled
        .iter()
        .filter(|name| available.contains(name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    result.sort();
    result.dedup();
    result
}

pub(crate) fn enabled_tool_definitions(
    available: &[McpToolInfo],
    disabled: &[String],
) -> Vec<ToolDefinition> {
    let disabled = disabled.iter().collect::<BTreeSet<_>>();
    available
        .iter()
        .filter(|tool| !disabled.contains(&tool.name))
        .map(|tool| ToolDefinition {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
            read_only: tool.read_only,
            destructive: tool.destructive,
        })
        .collect()
}

pub(crate) fn selected_tool_names(tools: &[McpToolInfo], selected: &[usize]) -> Vec<String> {
    let mut names = selected
        .iter()
        .filter_map(|index| tools.get(*index))
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

pub(crate) async fn handle_mcp_command(
    config: &mut crate::config::Config,
    path: &Path,
) -> Result<()> {
    loop {
        let mut labels = vec!["Добавить MCP-сервер".to_owned()];
        for server in &config.mcp.servers {
            let state = match fetch_tools(&server.url).await {
                Ok(tools) => format!("доступен, {} tools", tools.len()),
                Err(_) => "недоступен".to_owned(),
            };
            labels.push(format!("{} — {} ({state})", server.id, server.url));
        }
        labels.push("Закрыть".to_owned());
        let Some(choice) = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("MCP-серверы")
            .items(&labels)
            .default(0)
            .interact_opt()?
        else {
            return Ok(());
        };
        if choice == 0 {
            add_mcp_server(config, path).await?;
        } else if choice == labels.len() - 1 {
            return Ok(());
        } else {
            manage_mcp_server(config, path, choice - 1).await?;
        }
    }
}

async fn connect_and_list(url: &str) -> Result<(String, Vec<McpToolInfo>)> {
    let normalized = validate_mcp_url(url)?;
    let tools = fetch_tools(&normalized).await?;
    Ok((normalized, tools))
}

fn prompt_mcp_url() -> Result<Option<String>> {
    let value = Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("Адрес MCP-сервера (например, {MCP_SERVER_URL})"))
        .allow_empty(true)
        .interact_text()?;
    let value = value.trim().to_owned();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn prompt_mcp_server_id() -> Result<Option<String>> {
    let value = Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt("ID MCP-сервера (например, github)")
        .allow_empty(true)
        .interact_text()?;
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Ok(None);
    }
    crate::config::validate_mcp_server_id(&value)?;
    Ok(Some(value))
}

async fn add_mcp_server(config: &mut crate::config::Config, path: &Path) -> Result<()> {
    let Some(id) = prompt_mcp_server_id()? else {
        return Ok(());
    };
    anyhow::ensure!(
        !config.mcp.servers.iter().any(|server| server.id == id),
        "MCP-сервер с ID «{id}» уже зарегистрирован"
    );
    let Some(candidate) = prompt_mcp_url()? else {
        return Ok(());
    };
    match connect_and_list(&candidate).await {
        Ok((url, _tools)) => {
            config.mcp.servers.push(crate::config::McpServerConfig {
                id,
                url,
                disabled_tools: Vec::new(),
            });
            config
                .mcp
                .servers
                .sort_by(|left, right| left.id.cmp(&right.id));
            config.save(path)?;
            println!(
                "{}",
                style("MCP-сервер зарегистрирован; все tools включены.").green()
            );
        }
        Err(error) => println!("{} {error:#}", style("Не удалось подключиться:").red()),
    }
    Ok(())
}

async fn manage_mcp_server(
    config: &mut crate::config::Config,
    path: &Path,
    index: usize,
) -> Result<()> {
    loop {
        let Some(server) = config.mcp.servers.get(index).cloned() else {
            return Ok(());
        };
        let tools = match fetch_tools(&server.url).await {
            Ok(tools) => Some(tools),
            Err(error) => {
                println!("{} {error:#}", style("MCP-сервер недоступен:").red());
                None
            }
        };
        let actions = [
            "Включить/выключить инструменты",
            "Изменить адрес",
            "Удалить сервер",
            "Назад",
        ];
        let Some(action) = Select::with_theme(&ColorfulTheme::default())
            .with_prompt(format!("MCP-сервер {}", server.id))
            .items(&actions)
            .default(0)
            .interact_opt()?
        else {
            return Ok(());
        };
        match action {
            0 => {
                if let Some(tools) = tools.as_deref() {
                    select_mcp_tools(&mut config.mcp.servers[index], tools)?;
                    config.save(path)?;
                }
            }
            1 => {
                let Some(candidate) = prompt_mcp_url()? else {
                    continue;
                };
                match connect_and_list(&candidate).await {
                    Ok((normalized, _)) => {
                        config.mcp.servers[index].url = normalized;
                        config.save(path)?;
                        println!("{}", style("Адрес MCP-сервера изменён.").green());
                    }
                    Err(error) => println!("{} {error:#}", style("Не удалось подключиться:").red()),
                }
            }
            2 => {
                config.mcp.servers.remove(index);
                config.save(path)?;
                println!("{}", style("MCP-сервер удалён.").yellow());
                return Ok(());
            }
            _ => return Ok(()),
        }
    }
}

fn select_mcp_tools(
    server: &mut crate::config::McpServerConfig,
    tools: &[McpToolInfo],
) -> Result<()> {
    if tools.is_empty() {
        println!("{}", style("Сервер не объявил инструментов.").dim());
        return Ok(());
    }
    let disabled = reconcile_disabled_tools(tools, &server.disabled_tools);
    let labels = tools
        .iter()
        .map(|tool| {
            tool.description.as_deref().map_or_else(
                || tool.name.clone(),
                |description| format!("{} — {description}", tool.name),
            )
        })
        .collect::<Vec<_>>();
    let defaults = tools
        .iter()
        .map(|tool| !disabled.iter().any(|name| name == &tool.name))
        .collect::<Vec<_>>();
    let Some(selected) = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Выберите включённые инструменты (пробел — переключить)")
        .items(&labels)
        .defaults(&defaults)
        .interact_opt()?
    else {
        println!("{}", style("Выбор инструментов отменён.").dim());
        return Ok(());
    };
    let enabled = selected_tool_names(tools, &selected)
        .into_iter()
        .collect::<BTreeSet<_>>();
    server.disabled_tools = tools
        .iter()
        .filter(|tool| !enabled.contains(&tool.name))
        .map(|tool| tool.name.clone())
        .collect();
    server.disabled_tools.sort();
    println!(
        "{} {}",
        style("Отключённые инструменты:").yellow(),
        if server.disabled_tools.is_empty() {
            "нет".to_owned()
        } else {
            server.disabled_tools.join(", ")
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;
    use tokio::net::TcpListener;

    #[derive(Default)]
    struct RecordingLogger(Mutex<Vec<String>>);

    impl McpCallLogger for RecordingLogger {
        fn log(&self, event: &McpCallEvent<'_>) {
            self.0
                .lock()
                .unwrap()
                .push(serde_json::to_string(event).unwrap());
        }
    }

    #[test]
    fn validates_http_urls() {
        assert_eq!(
            validate_mcp_url(" http://localhost:8000/mcp ").unwrap(),
            "http://localhost:8000/mcp"
        );
        assert!(validate_mcp_url("localhost:8000/mcp").is_err());
        assert!(validate_mcp_url("file:///tmp/mcp").is_err());
    }

    #[test]
    fn removes_duplicate_and_stale_disabled_tools() {
        let available = vec![McpToolInfo {
            name: "echo".into(),
            description: None,
            input_schema: json!({}),
            read_only: true,
            destructive: false,
        }];
        assert_eq!(
            reconcile_disabled_tools(&available, &["old".into(), "echo".into(), "echo".into()]),
            vec!["echo"]
        );
    }

    #[test]
    fn qualifies_tool_names_without_ambiguity() {
        assert_eq!(qualify_tool_name("github", "search"), "github__search");
        assert!(crate::config::validate_mcp_server_id("bad__id").is_err());
    }

    #[test]
    fn selected_tool_names_are_sorted_and_deduplicated() {
        let tools = vec![
            McpToolInfo {
                name: "zeta".into(),
                description: None,
                input_schema: json!({}),
                read_only: true,
                destructive: false,
            },
            McpToolInfo {
                name: "echo".into(),
                description: None,
                input_schema: json!({}),
                read_only: true,
                destructive: false,
            },
        ];
        assert_eq!(
            selected_tool_names(&tools, &[0, 1, 0]),
            vec!["echo", "zeta"]
        );
    }

    #[test]
    fn enabled_tool_definitions_preserve_schema_and_annotations() {
        let tools = vec![
            McpToolInfo {
                name: "write".into(),
                description: Some("Writes data".into()),
                input_schema: json!({"type": "object", "required": ["title"]}),
                read_only: false,
                destructive: false,
            },
            McpToolInfo {
                name: "hidden".into(),
                description: None,
                input_schema: json!({"type": "object"}),
                read_only: true,
                destructive: false,
            },
        ];
        let selected = enabled_tool_definitions(&tools, &["hidden".into(), "stale".into()]);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "write");
        assert_eq!(selected[0].input_schema["required"][0], "title");
        assert!(!selected[0].read_only);
    }

    #[tokio::test]
    async fn named_servers_expose_only_their_tool_groups_without_echo() {
        let cases = [
            (
                McpServerKind::Github,
                vec!["repository_metadata", "project_activity"],
            ),
            (
                McpServerKind::Reporting,
                vec!["calculate_github_metrics", "render_github_report"],
            ),
            (McpServerKind::Workspace, vec!["save_report"]),
            (McpServerKind::Calendar, vec!["create_event", "list_events"]),
        ];
        for (kind, expected) in cases {
            let listener = match TcpListener::bind("127.0.0.1:0").await {
                Ok(listener) => listener,
                Err(error) => {
                    eprintln!("loopback MCP test skipped: {error}");
                    return;
                }
            };
            let address = listener.local_addr().unwrap();
            let directory = tempfile::tempdir().unwrap();
            let task = tokio::spawn(serve_group_for_test(
                kind,
                listener,
                directory.path().to_path_buf(),
            ));
            let tools = fetch_tools(&format!("http://{address}/mcp")).await.unwrap();
            assert_eq!(
                tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<BTreeSet<_>>(),
                expected.into_iter().collect::<BTreeSet<_>>()
            );
            assert!(tools.iter().all(|tool| tool.name != "echo"));
            task.abort();
        }
    }

    #[test]
    fn tool_outcomes_are_explicit_and_serializable() {
        assert_eq!(
            serde_json::to_value(ToolOutcome::Success).unwrap(),
            json!("success")
        );
        assert_eq!(
            serde_json::to_value(ToolOutcome::Failed).unwrap(),
            json!("failed")
        );
        assert_eq!(
            serde_json::to_value(ToolOutcome::Unknown).unwrap(),
            json!("unknown")
        );
    }

    #[tokio::test]
    async fn router_qualifies_colliding_tools_and_isolates_unavailable_servers() {
        let first = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("loopback MCP test skipped: {error}");
                return;
            }
        };
        let second = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("loopback MCP test skipped: {error}");
                return;
            }
        };
        let first_address = first.local_addr().unwrap();
        let second_address = second.local_addr().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let task_one = tokio::spawn(serve_group_for_test(
            McpServerKind::Reporting,
            first,
            directory.path().to_path_buf(),
        ));
        let task_two = tokio::spawn(serve_group_for_test(
            McpServerKind::Reporting,
            second,
            directory.path().to_path_buf(),
        ));
        let router = McpRouter::connect(&[
            crate::config::McpServerConfig {
                id: "one".into(),
                url: format!("http://{first_address}/mcp"),
                disabled_tools: vec!["render_github_report".into()],
            },
            crate::config::McpServerConfig {
                id: "two".into(),
                url: format!("http://{second_address}/mcp"),
                disabled_tools: vec![],
            },
            crate::config::McpServerConfig {
                id: "offline".into(),
                url: "http://127.0.0.1:1/mcp".into(),
                disabled_tools: vec![],
            },
        ])
        .await;
        assert_eq!(
            router
                .route("one__calculate_github_metrics")
                .unwrap()
                .server_id,
            "one"
        );
        assert_eq!(
            router
                .route("two__calculate_github_metrics")
                .unwrap()
                .server_id,
            "two"
        );
        assert!(router.route("one__render_github_report").is_none());
        assert!(router
            .warnings()
            .iter()
            .any(|warning| warning.starts_with("offline:")));
        task_one.abort();
        task_two.abort();
    }

    #[tokio::test]
    async fn router_logs_metadata_without_arguments_or_results() {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("loopback MCP logging test skipped: {error}");
                return;
            }
        };
        let address = listener.local_addr().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let task = tokio::spawn(serve_group_for_test(
            McpServerKind::Reporting,
            listener,
            directory.path().to_path_buf(),
        ));
        let logger = Arc::new(RecordingLogger::default());
        let router = McpRouter::connect_with_logger(
            &[crate::config::McpServerConfig {
                id: "reporting".into(),
                url: format!("http://{address}/mcp"),
                disabled_tools: Vec::new(),
            }],
            logger.clone(),
        )
        .await;
        let _ = router
            .execute(&ToolCall {
                id: "pipeline-1:step-2".into(),
                name: "reporting__calculate_github_metrics".into(),
                arguments: json!({"secret_marker": "DO_NOT_LOG"}),
            })
            .await;
        let events = logger.0.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].contains("mcp_call_start"));
        assert!(events[1].contains("mcp_call_finish"));
        let start: serde_json::Value = serde_json::from_str(&events[0]).unwrap();
        let finish: serde_json::Value = serde_json::from_str(&events[1]).unwrap();
        assert_eq!(start["correlation_id"], finish["correlation_id"]);
        assert_eq!(start["step_id"], "step-2");
        assert!(events.iter().all(|event| event.contains("reporting")));
        assert!(events.iter().all(|event| !event.contains("DO_NOT_LOG")));
        task.abort();
    }
}
