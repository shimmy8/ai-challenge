use anyhow::{anyhow, Context, Result};
use rmcp::Json;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, tower::StreamableHttpService,
        StreamableHttpServerConfig,
    },
    ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use super::calendar::{CalDavClient, CalendarEventRequest, CalendarEventResult};
use super::{MCP_SERVER_ADDR, MCP_SERVER_URL};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct EchoRequest {
    /// Text to return unchanged.
    text: String,
}

#[derive(Debug, Clone)]
struct EchoServer {
    tool_router: ToolRouter<Self>,
}

impl EchoServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router(router = tool_router)]
impl EchoServer {
    #[tool(
        name = "echo",
        description = "Возвращает переданный текст без изменений",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn echo(&self, Parameters(request): Parameters<EchoRequest>) -> String {
        request.text
    }

    #[tool(
        name = "create_calendar_event",
        description = "Создаёт подтверждённое личное событие в Яндекс Календаре через CalDAV",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn create_calendar_event(
        &self,
        Parameters(request): Parameters<CalendarEventRequest>,
    ) -> Result<Json<CalendarEventResult>, rmcp::ErrorData> {
        let client = CalDavClient::from_env().map_err(|error| {
            let message = safe_calendar_error(error);
            eprintln!("MCP calendar: {message}");
            rmcp::ErrorData::invalid_params(message, None)
        })?;
        client
            .create_event(&request)
            .await
            .map(Json)
            .map_err(|error| {
                let message = safe_calendar_error(error);
                eprintln!("MCP calendar: {message}");
                rmcp::ErrorData::internal_error(message, None)
            })
    }
}

fn safe_calendar_error(error: anyhow::Error) -> String {
    let message = error.to_string();
    if message.contains("YANDEX_CALDAV_PASSWORD")
        || message.contains("YANDEX_CALDAV_USERNAME")
        || message.contains("парол")
    {
        "Не удалось настроить доступ к Яндекс Календарю: проверьте логин и пароль приложения."
            .into()
    } else {
        message
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for EchoServer {}

pub(crate) async fn run_mcp_server() -> Result<()> {
    let listener = TcpListener::bind(MCP_SERVER_ADDR)
        .await
        .with_context(|| format!("не удалось запустить MCP-сервер на {MCP_SERVER_ADDR}"))?;
    println!("MCP-сервер запущен: {MCP_SERVER_URL}");
    serve_mcp_listener(listener).await
}

pub(crate) async fn serve_mcp_listener(listener: TcpListener) -> Result<()> {
    let service = StreamableHttpService::new(
        || Ok(EchoServer::new()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    axum::serve(listener, router)
        .await
        .context("MCP-сервер завершился с ошибкой")
}
