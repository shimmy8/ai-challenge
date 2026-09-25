use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, FixedOffset, TimeZone};
use reqwest::Client;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, tower::StreamableHttpService,
        StreamableHttpServerConfig,
    },
    Json, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{net::SocketAddr, path::PathBuf};
use tokio::net::TcpListener;

use super::{
    calculate_github_metrics, render_github_report, save_report_in_directory, CalDavClient,
    CalculateGithubMetricsRequest, CalendarDigestEvent, CalendarEventRequest, CalendarEventResult,
    GithubActivityRequest, GithubClient, GithubMetrics, GithubProjectActivity,
    GithubRepositoryMetadata, GithubRepositoryRequest, McpServerKind, RenderGithubReportRequest,
    SaveReportRequest, SaveReportResult, TelegramClient, TelegramOutcome, ToolOutcome,
};
use crate::{send_request, AgentSettings, Config, Message, Provider, RequestOptions};

const AI_SYSTEM_INSTRUCTIONS: &str = "Преобразуй только явно переданный JSON-вход по инструкциям текущего вызова. Не используй внешние инструменты, не выполняй действия и не добавляй неизвестные факты.";

fn tool_error(error: anyhow::Error) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(safe_error(error), None)
}

fn safe_error(error: anyhow::Error) -> String {
    let message = error.to_string();
    let lower = message.to_lowercase();
    if lower.contains("password")
        || lower.contains("парол")
        || message.contains("TELEGRAM_BOT_TOKEN")
        || message.contains("TELEGRAM_CHAT_ID")
        || message.contains("YANDEX_CALDAV_")
    {
        "не удалось настроить внешний сервис".into()
    } else {
        message.chars().take(500).collect()
    }
}

macro_rules! impl_logged_server_handler {
    ($server:ty, $server_id:literal) => {
        #[tool_handler(router = self.tool_router)]
        impl ServerHandler for $server {
            async fn call_tool(
                &self,
                request: rmcp::model::CallToolRequestParams,
                context: rmcp::service::RequestContext<rmcp::RoleServer>,
            ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
                let tool = request.name.to_string();
                let correlation_id = context
                    .meta
                    .get_traceparent()
                    .map(str::to_owned)
                    .unwrap_or_else(|| context.id.to_string());
                let step_id = context
                    .meta
                    .get_baggage()
                    .and_then(|value| value.strip_prefix("fox-step="))
                    .map(str::to_owned);
                super::log_local_call(
                    "mcp_server_call_start",
                    &correlation_id,
                    $server_id,
                    &tool,
                    step_id.as_deref(),
                    None,
                    None,
                );
                let started = std::time::Instant::now();
                let call =
                    rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
                let result = self.tool_router.call(call).await;
                let outcome = match &result {
                    Ok(rmcp::model::CallToolResponse::Complete(response))
                        if response.is_error.unwrap_or(false) =>
                    {
                        ToolOutcome::Failed
                    }
                    Ok(rmcp::model::CallToolResponse::Complete(_)) => ToolOutcome::Success,
                    Ok(_) => ToolOutcome::Unknown,
                    Err(_) => ToolOutcome::Failed,
                };
                super::log_local_call(
                    "mcp_server_call_finish",
                    &correlation_id,
                    $server_id,
                    &tool,
                    step_id.as_deref(),
                    Some(outcome),
                    Some(started.elapsed().as_millis()),
                );
                result
            }
        }
    };
}

#[derive(Clone)]
struct GithubServer {
    tool_router: ToolRouter<Self>,
    client: GithubClient,
}

impl GithubServer {
    fn new(client: GithubClient) -> Self {
        Self {
            tool_router: Self::tool_router(),
            client,
        }
    }
}

#[tool_router(router = tool_router)]
impl GithubServer {
    #[tool(
        name = "repository_metadata",
        description = "Возвращает публичные метаданные и языки GitHub-репозитория",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn repository_metadata(
        &self,
        Parameters(request): Parameters<GithubRepositoryRequest>,
    ) -> Result<Json<GithubRepositoryMetadata>, rmcp::ErrorData> {
        self.client
            .repository_metadata(&request)
            .await
            .map(Json)
            .map_err(tool_error)
    }

    #[tool(
        name = "project_activity",
        description = "Собирает публичную активность GitHub-репозитория за период",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn project_activity(
        &self,
        Parameters(request): Parameters<GithubActivityRequest>,
    ) -> Result<Json<GithubProjectActivity>, rmcp::ErrorData> {
        self.client
            .project_activity(&request)
            .await
            .map(Json)
            .map_err(tool_error)
    }
}

impl_logged_server_handler!(GithubServer, "github");

#[derive(Clone)]
struct ReportingServer {
    tool_router: ToolRouter<Self>,
}

impl ReportingServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router(router = tool_router)]
impl ReportingServer {
    #[tool(
        name = "calculate_github_metrics",
        description = "Детерминированно рассчитывает метрики GitHub-проекта",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn calculate_github_metrics(
        &self,
        Parameters(request): Parameters<CalculateGithubMetricsRequest>,
    ) -> Result<Json<GithubMetrics>, rmcp::ErrorData> {
        calculate_github_metrics(&request)
            .map(Json)
            .map_err(tool_error)
    }

    #[tool(
        name = "render_github_report",
        description = "Формирует Markdown-отчёт из GitHub-данных и метрик",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn render_github_report(
        &self,
        Parameters(request): Parameters<RenderGithubReportRequest>,
    ) -> Result<Json<String>, rmcp::ErrorData> {
        render_github_report(&request).map(Json).map_err(tool_error)
    }
}

impl_logged_server_handler!(ReportingServer, "reporting");

#[derive(Clone)]
struct WorkspaceServer {
    tool_router: ToolRouter<Self>,
    root: PathBuf,
}

impl WorkspaceServer {
    fn new(root: PathBuf) -> Self {
        Self {
            tool_router: Self::tool_router(),
            root,
        }
    }
}

#[tool_router(router = tool_router)]
impl WorkspaceServer {
    #[tool(
        name = "save_report",
        description = "Сохраняет UTF-8 отчёт в безопасное имя файла рабочего каталога",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    fn save_report(
        &self,
        Parameters(request): Parameters<SaveReportRequest>,
    ) -> Result<Json<SaveReportResult>, rmcp::ErrorData> {
        save_report_in_directory(&self.root, &request)
            .map(Json)
            .map_err(tool_error)
    }
}

impl_logged_server_handler!(WorkspaceServer, "workspace");

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListEventsRequest {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    relative_to: Option<String>,
    #[serde(default)]
    target_day: Option<String>,
    #[serde(default = "default_timezone")]
    timezone: String,
}

fn default_timezone() -> String {
    "Europe/Moscow".into()
}

fn resolve_calendar_interval(
    request: &ListEventsRequest,
) -> Result<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
    match (
        &request.from,
        &request.to,
        &request.relative_to,
        &request.target_day,
    ) {
        (Some(from), Some(to), None, None) => {
            let from = DateTime::parse_from_rfc3339(from)
                .context("from должен быть RFC 3339 со смещением")?;
            let to =
                DateTime::parse_from_rfc3339(to).context("to должен быть RFC 3339 со смещением")?;
            anyhow::ensure!(to > from, "интервал календаря должен быть положительным");
            Ok((from, to))
        }
        (None, None, Some(relative_to), Some(target_day)) => {
            anyhow::ensure!(
                target_day == "tomorrow",
                "поддерживается только target_day=tomorrow"
            );
            anyhow::ensure!(
                request.timezone == "Europe/Moscow",
                "поддерживается только Europe/Moscow"
            );
            let reference = DateTime::parse_from_rfc3339(relative_to)
                .context("relative_to должен быть RFC 3339 со смещением")?;
            let zone = FixedOffset::east_opt(3 * 60 * 60).expect("valid Moscow offset");
            let day = reference.with_timezone(&zone).date_naive() + Duration::days(1);
            let from = zone
                .from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("valid midnight"))
                .single()
                .expect("fixed offset is unique");
            Ok((from, from + Duration::days(1)))
        }
        _ => anyhow::bail!("задайте либо from/to, либо relative_to/target_day"),
    }
}

#[derive(Clone)]
struct CalendarServer {
    tool_router: ToolRouter<Self>,
}

impl CalendarServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router(router = tool_router)]
impl CalendarServer {
    #[tool(
        name = "create_event",
        description = "Создаёт подтверждённое событие в Яндекс Календаре",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn create_event(
        &self,
        Parameters(request): Parameters<CalendarEventRequest>,
    ) -> Result<Json<CalendarEventResult>, rmcp::ErrorData> {
        let client = CalDavClient::from_env().map_err(tool_error)?;
        client
            .create_event(&request)
            .await
            .map(Json)
            .map_err(tool_error)
    }

    #[tool(
        name = "list_events",
        description = "Читает события Яндекс Календаря за абсолютный или относительный интервал",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn list_events(
        &self,
        Parameters(request): Parameters<ListEventsRequest>,
    ) -> Result<Json<Vec<CalendarDigestEvent>>, rmcp::ErrorData> {
        let (from, to) = resolve_calendar_interval(&request).map_err(tool_error)?;
        let client = CalDavClient::from_env().map_err(tool_error)?;
        client
            .list_events(from, to)
            .await
            .map(Json)
            .map_err(tool_error)
    }
}

impl_logged_server_handler!(CalendarServer, "calendar");

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GenerateTextRequest {
    instructions: String,
    input: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct GenerateTextResult {
    text: String,
    provider: String,
    model: String,
}

#[derive(Clone)]
struct AiServer {
    tool_router: ToolRouter<Self>,
    http: Client,
    settings: AgentSettings,
}

impl AiServer {
    fn from_config(config: &Config) -> Result<Self> {
        let provider = config.last_provider.unwrap_or(Provider::Openai);
        let settings = isolated_ai_settings(AgentSettings::from_config(config, provider, None)?);
        Ok(Self {
            tool_router: Self::tool_router(),
            http: Client::builder().user_agent("fox-llm-ai-mcp/0.1").build()?,
            settings,
        })
    }
}

fn isolated_ai_settings(mut settings: AgentSettings) -> AgentSettings {
    settings.instructions = Some(AI_SYSTEM_INSTRUCTIONS.into());
    settings
}

fn prepare_ai_messages(request: &GenerateTextRequest) -> Result<Vec<Message>> {
    anyhow::ensure!(
        !request.instructions.trim().is_empty() && request.instructions.len() <= 10_000,
        "instructions пусты или превышают допустимый размер"
    );
    let serialized = serde_json::to_string(&request.input)?;
    anyhow::ensure!(
        !serialized.is_empty() && serialized.len() <= 100_000,
        "AI input вне допустимого размера"
    );
    Ok(vec![Message {
        role: "user".into(),
        content: format!(
            "Инструкции:\n{}\n\nВход JSON:\n{}",
            request.instructions, serialized
        ),
    }])
}

fn safe_ai_error(error: anyhow::Error, api_key: &str) -> rmcp::ErrorData {
    let message = error.to_string();
    if !api_key.is_empty() && message.contains(api_key) {
        return rmcp::ErrorData::internal_error("ошибка AI-провайдера", None);
    }
    tool_error(error)
}

#[tool_router(router = tool_router)]
impl AiServer {
    #[tool(
        name = "generate_text",
        description = "Изолированно преобразует JSON-вход в текст по явным инструкциям",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn generate_text(
        &self,
        Parameters(request): Parameters<GenerateTextRequest>,
    ) -> Result<Json<GenerateTextResult>, rmcp::ErrorData> {
        let messages = prepare_ai_messages(&request)
            .map_err(|error| rmcp::ErrorData::invalid_params(error.to_string(), None))?;
        let answer = send_request(
            &self.http,
            &self.settings,
            &messages,
            &RequestOptions::default(),
        )
        .await
        .map_err(|error| safe_ai_error(error, &self.settings.api_key))?;
        if !answer.tool_calls.is_empty() {
            return Err(rmcp::ErrorData::internal_error(
                "AI-сервер получил tool call",
                None,
            ));
        }
        if answer.text.trim().is_empty() {
            return Err(rmcp::ErrorData::internal_error(
                "модель вернула пустой текст",
                None,
            ));
        }
        Ok(Json(GenerateTextResult {
            text: answer.text,
            provider: self.settings.provider.to_string(),
            model: self.settings.model.clone(),
        }))
    }
}

impl_logged_server_handler!(AiServer, "ai");

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendMessageRequest {
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct SendMessageResult {
    outcome: String,
    message_id: Option<i64>,
    error: Option<String>,
}

#[derive(Clone)]
struct TelegramServer {
    tool_router: ToolRouter<Self>,
    client: TelegramClient,
}

impl TelegramServer {
    fn new(client: TelegramClient) -> Self {
        Self {
            tool_router: Self::tool_router(),
            client,
        }
    }
}

#[tool_router(router = tool_router)]
impl TelegramServer {
    #[tool(
        name = "send_message",
        description = "Отправляет текст в локально настроенный Telegram-чат",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn send_message(
        &self,
        Parameters(request): Parameters<SendMessageRequest>,
    ) -> Result<Json<SendMessageResult>, rmcp::ErrorData> {
        if request.text.trim().is_empty() || request.text.len() > 4096 {
            return Err(rmcp::ErrorData::invalid_params(
                "текст Telegram вне допустимого размера",
                None,
            ));
        }
        let result = match self.client.send(&request.text).await.map_err(tool_error)? {
            TelegramOutcome::Delivered(message_id) => SendMessageResult {
                outcome: "delivered".into(),
                message_id,
                error: None,
            },
            TelegramOutcome::Failed(error) => SendMessageResult {
                outcome: "failed".into(),
                message_id: None,
                error: Some(error),
            },
            TelegramOutcome::Unknown(error) => SendMessageResult {
                outcome: "unknown".into(),
                message_id: None,
                error: Some(error),
            },
        };
        Ok(Json(result))
    }
}

impl_logged_server_handler!(TelegramServer, "telegram");

macro_rules! serve_handler {
    ($listener:expr, $factory:expr) => {{
        let factory = $factory;
        let service = StreamableHttpService::new(
            move || Ok(factory.clone()),
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default(),
        );
        let router = axum::Router::new().nest_service("/mcp", service);
        axum::serve($listener, router)
            .await
            .context("MCP-сервер завершился с ошибкой")
    }};
}

pub(crate) async fn serve_group(
    kind: McpServerKind,
    listener: TcpListener,
    config: &Config,
    workspace: PathBuf,
) -> Result<()> {
    match kind {
        McpServerKind::Github => {
            serve_handler!(listener, GithubServer::new(GithubClient::public()?))
        }
        McpServerKind::Reporting => serve_handler!(listener, ReportingServer::new()),
        McpServerKind::Workspace => serve_handler!(listener, WorkspaceServer::new(workspace)),
        McpServerKind::Calendar => serve_handler!(listener, CalendarServer::new()),
        McpServerKind::Ai => serve_handler!(listener, AiServer::from_config(config)?),
        McpServerKind::Telegram => {
            serve_handler!(listener, TelegramServer::new(TelegramClient::from_env()?))
        }
        McpServerKind::Orchestrator => Err(anyhow!("orchestrator запускается отдельным runtime")),
    }
}

#[cfg(test)]
pub(crate) async fn serve_group_for_test(
    kind: McpServerKind,
    listener: TcpListener,
    workspace: PathBuf,
) -> Result<()> {
    serve_group(kind, listener, &Config::default(), workspace).await
}

#[cfg(test)]
pub(crate) async fn serve_github_for_test(
    listener: TcpListener,
    client: GithubClient,
) -> Result<()> {
    serve_handler!(listener, GithubServer::new(client))
}

pub(crate) async fn bind_and_serve_group(
    kind: McpServerKind,
    address: SocketAddr,
    config: &Config,
    workspace: PathBuf,
) -> Result<()> {
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("не удалось запустить MCP-сервер {kind} на {address}"))?;
    super::log_server_started(kind, address);
    serve_group(kind, listener, config, workspace).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn tomorrow_interval_uses_moscow_calendar_boundaries() {
        let request = ListEventsRequest {
            from: None,
            to: None,
            relative_to: Some("2026-09-25T22:30:00Z".into()),
            target_day: Some("tomorrow".into()),
            timezone: "Europe/Moscow".into(),
        };
        let (from, to) = resolve_calendar_interval(&request).unwrap();
        assert_eq!(from.to_rfc3339(), "2026-09-27T00:00:00+03:00");
        assert_eq!(to - from, Duration::days(1));
        assert_eq!(from.hour(), 0);
    }

    #[test]
    fn calendar_interval_rejects_mixed_or_unknown_relative_inputs() {
        let mixed = ListEventsRequest {
            from: Some("2026-09-25T00:00:00Z".into()),
            to: Some("2026-09-26T00:00:00Z".into()),
            relative_to: Some("2026-09-25T00:00:00Z".into()),
            target_day: Some("tomorrow".into()),
            timezone: "Europe/Moscow".into(),
        };
        assert!(resolve_calendar_interval(&mixed).is_err());
        let wrong_zone = ListEventsRequest {
            from: None,
            to: None,
            relative_to: Some("2026-09-25T00:00:00Z".into()),
            target_day: Some("tomorrow".into()),
            timezone: "UTC".into(),
        };
        assert!(resolve_calendar_interval(&wrong_zone).is_err());
    }

    #[test]
    fn ai_and_telegram_requests_reject_destination_and_credentials() {
        assert!(
            serde_json::from_value::<GenerateTextRequest>(serde_json::json!({
                "instructions": "summary",
                "input": {},
                "api_key": "secret"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SendMessageRequest>(serde_json::json!({
                "text": "hello",
                "chat_id": "other"
            }))
            .is_err()
        );
        assert_eq!(
            safe_error(anyhow!("не задан TELEGRAM_BOT_TOKEN")),
            "не удалось настроить внешний сервис"
        );
    }

    #[test]
    fn ai_request_is_bounded_isolated_and_redacts_provider_key() {
        let request = GenerateTextRequest {
            instructions: "Составь сводку".into(),
            input: serde_json::json!({"events": [1, 2]}),
        };
        let messages = prepare_ai_messages(&request).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert!(messages[0].content.contains("events"));
        assert!(prepare_ai_messages(&GenerateTextRequest {
            instructions: " ".into(),
            input: Value::Null,
        })
        .is_err());

        let settings = isolated_ai_settings(AgentSettings {
            provider: Provider::Openai,
            api_key: "sk-private-marker".into(),
            model: "test".into(),
            temperature: 0.0,
            instructions: Some("история пользователя".into()),
            compression_strategy: crate::CompressionStrategy::Summary,
            context_messages: 99,
        });
        assert_eq!(
            settings.instructions.as_deref(),
            Some(AI_SYSTEM_INSTRUCTIONS)
        );
        let error = safe_ai_error(
            anyhow!("provider rejected sk-private-marker"),
            &settings.api_key,
        );
        assert!(!error.message.contains("sk-private-marker"));
    }
}
