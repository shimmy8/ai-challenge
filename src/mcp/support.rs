use super::{McpRouter, SharedToolExecutor, ToolCall, ToolExecutor};
use crate::config::McpServerConfig;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, tower::StreamableHttpService,
        StreamableHttpServerConfig,
    },
    Json, ServerHandler,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

const SCHEMA_VERSION: i64 = 1;
const SUPPORT_SERVER_ID: &str = "support";
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(300);
pub(crate) const MAX_TICKET_MESSAGES: usize = 40;
pub(crate) const MAX_TICKET_CONTEXT_CHARS: usize = 4000;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateTicketRequest {
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) subject: String,
    pub(crate) description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TicketIdRequest {
    pub(crate) ticket_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListMessagesRequest {
    pub(crate) ticket_id: String,
    #[serde(default = "default_message_limit")]
    pub(crate) limit: usize,
    #[serde(default = "default_char_budget")]
    pub(crate) char_budget: usize,
}

fn default_message_limit() -> usize {
    MAX_TICKET_MESSAGES
}

fn default_char_budget() -> usize {
    MAX_TICKET_CONTEXT_CHARS
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AddMessageRequest {
    pub(crate) ticket_id: String,
    pub(crate) role: String,
    pub(crate) content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct SupportUser {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct SupportTicket {
    pub(crate) id: String,
    pub(crate) subject: String,
    pub(crate) initial_description: String,
    pub(crate) status: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) user: SupportUser,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct SupportMessage {
    pub(crate) id: i64,
    pub(crate) ticket_id: String,
    pub(crate) role: String,
    pub(crate) content: String,
    pub(crate) created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct CreatedTicket {
    pub(crate) ticket: SupportTicket,
    pub(crate) initial_message: SupportMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct TicketMessages {
    pub(crate) messages: Vec<SupportMessage>,
    pub(crate) truncated: bool,
}

pub(crate) struct SupportStore {
    connection: Connection,
}

impl SupportStore {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("не удалось открыть support-базу {}", path.display()))?;
        set_private_database_permissions(path)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        anyhow::ensure!(
            version <= SCHEMA_VERSION,
            "support-база имеет неподдерживаемую версию схемы {version}"
        );
        if version == 0 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE users (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   name TEXT NOT NULL,
                   normalized_email TEXT NOT NULL UNIQUE,
                   created_at TEXT NOT NULL
                 );
                 CREATE TABLE tickets (
                   id TEXT PRIMARY KEY,
                   user_id INTEGER NOT NULL REFERENCES users(id),
                   subject TEXT NOT NULL,
                   status TEXT NOT NULL CHECK(status IN ('open','closed')),
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE messages (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   ticket_id TEXT NOT NULL REFERENCES tickets(id),
                   role TEXT NOT NULL CHECK(role IN ('user','assistant')),
                   content TEXT NOT NULL,
                   created_at TEXT NOT NULL
                 );
                 CREATE INDEX messages_ticket_order ON messages(ticket_id, id);
                 PRAGMA user_version = 1;
                 COMMIT;",
            )?;
        }
        Ok(Self { connection })
    }

    pub(crate) fn create_ticket(&mut self, request: &CreateTicketRequest) -> Result<CreatedTicket> {
        validate_ticket(request)?;
        let tx = self.connection.transaction()?;
        let email = request.email.trim().to_lowercase();
        let now = Utc::now().to_rfc3339();
        let user_id = match tx
            .query_row(
                "SELECT id FROM users WHERE normalized_email = ?1",
                [&email],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO users(name, normalized_email, created_at) VALUES(?1, ?2, ?3)",
                    params![request.name.trim(), email, now],
                )?;
                tx.last_insert_rowid()
            }
        };
        let ticket_id = format!("T-{}", uuid::Uuid::new_v4().simple());
        tx.execute(
            "INSERT INTO tickets(id, user_id, subject, status, created_at, updated_at) VALUES(?1, ?2, ?3, 'open', ?4, ?4)",
            params![ticket_id, user_id, request.subject.trim(), now],
        )?;
        tx.execute(
            "INSERT INTO messages(ticket_id, role, content, created_at) VALUES(?1, 'user', ?2, ?3)",
            params![ticket_id, request.description.trim(), now],
        )?;
        let message_id = tx.last_insert_rowid();
        let created = load_created_ticket(&tx, &ticket_id, message_id)?;
        tx.commit()?;
        Ok(created)
    }

    pub(crate) fn get_ticket(&self, ticket_id: &str) -> Result<SupportTicket> {
        self.connection
            .query_row(
                "SELECT t.id,t.subject,
                        (SELECT m.content FROM messages m WHERE m.ticket_id=t.id AND m.role='user' ORDER BY m.id LIMIT 1),
                        t.status,t.created_at,t.updated_at,u.id,u.name,u.normalized_email
                 FROM tickets t JOIN users u ON u.id=t.user_id WHERE t.id=?1",
                [ticket_id],
                map_ticket,
            )
            .optional()?
            .ok_or_else(|| anyhow!("тикет не найден"))
    }

    pub(crate) fn add_message(&mut self, request: &AddMessageRequest) -> Result<SupportMessage> {
        validate_message(request)?;
        let status = self
            .connection
            .query_row(
                "SELECT status FROM tickets WHERE id=?1",
                [&request.ticket_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("тикет не найден"))?;
        anyhow::ensure!(status == "open", "тикет закрыт");
        let now = Utc::now().to_rfc3339();
        let tx = self.connection.transaction()?;
        tx.execute(
            "INSERT INTO messages(ticket_id,role,content,created_at) VALUES(?1,?2,?3,?4)",
            params![request.ticket_id, request.role, request.content.trim(), now],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE tickets SET updated_at=?1 WHERE id=?2",
            params![now, request.ticket_id],
        )?;
        tx.commit()?;
        Ok(SupportMessage {
            id,
            ticket_id: request.ticket_id.clone(),
            role: request.role.clone(),
            content: request.content.trim().to_owned(),
            created_at: now,
        })
    }

    pub(crate) fn list_messages(&self, request: &ListMessagesRequest) -> Result<TicketMessages> {
        self.get_ticket(&request.ticket_id)?;
        anyhow::ensure!(
            request.limit > 0 && request.limit <= 200,
            "некорректный limit"
        );
        anyhow::ensure!(
            request.char_budget > 0 && request.char_budget <= 50_000,
            "некорректный char_budget"
        );
        let mut statement = self.connection.prepare(
            "SELECT id,ticket_id,role,content,created_at FROM messages WHERE ticket_id=?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let all = statement
            .query_map(params![request.ticket_id, request.limit + 1], map_message)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut selected = Vec::new();
        let mut used = 0;
        let mut truncated = all.len() > request.limit;
        for message in all.into_iter().take(request.limit) {
            let chars = message.content.chars().count();
            if used + chars > request.char_budget {
                truncated = true;
                break;
            }
            used += chars;
            selected.push(message);
        }
        selected.reverse();
        Ok(TicketMessages {
            messages: selected,
            truncated,
        })
    }
}

fn set_private_database_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn validate_ticket(request: &CreateTicketRequest) -> Result<()> {
    validate_text("имя", &request.name, 1, 120)?;
    validate_text("email", &request.email, 3, 254)?;
    let email = request.email.trim();
    let (local, domain) = email
        .split_once('@')
        .ok_or_else(|| anyhow!("email имеет неверный формат"))?;
    anyhow::ensure!(
        !local.is_empty()
            && !domain.is_empty()
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
            && !domain.contains('@')
            && !email.contains(char::is_whitespace),
        "email имеет неверный формат"
    );
    validate_text("тема", &request.subject, 1, 200)?;
    validate_text("описание", &request.description, 1, 8000)
}

fn validate_message(request: &AddMessageRequest) -> Result<()> {
    anyhow::ensure!(
        matches!(request.role.as_str(), "user" | "assistant"),
        "неверная роль сообщения"
    );
    validate_text("сообщение", &request.content, 1, 8000)
}

fn validate_text(name: &str, value: &str, min: usize, max: usize) -> Result<()> {
    let count = value.trim().chars().count();
    anyhow::ensure!(
        (min..=max).contains(&count),
        "поле «{name}» имеет неверную длину"
    );
    Ok(())
}

fn map_ticket(row: &rusqlite::Row<'_>) -> rusqlite::Result<SupportTicket> {
    Ok(SupportTicket {
        id: row.get(0)?,
        subject: row.get(1)?,
        initial_description: row.get(2)?,
        status: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        user: SupportUser {
            id: row.get(6)?,
            name: row.get(7)?,
            email: row.get(8)?,
        },
    })
}

fn map_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<SupportMessage> {
    Ok(SupportMessage {
        id: row.get(0)?,
        ticket_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        created_at: row.get(4)?,
    })
}

fn load_created_ticket(tx: &Transaction<'_>, id: &str, message_id: i64) -> Result<CreatedTicket> {
    let ticket = tx.query_row(
        "SELECT t.id,t.subject,
                (SELECT m.content FROM messages m WHERE m.ticket_id=t.id AND m.role='user' ORDER BY m.id LIMIT 1),
                t.status,t.created_at,t.updated_at,u.id,u.name,u.normalized_email
         FROM tickets t JOIN users u ON u.id=t.user_id WHERE t.id=?1",
        [id], map_ticket,
    )?;
    let initial_message = tx.query_row(
        "SELECT id,ticket_id,role,content,created_at FROM messages WHERE id=?1",
        [message_id],
        map_message,
    )?;
    Ok(CreatedTicket {
        ticket,
        initial_message,
    })
}

#[derive(Clone)]
struct SupportServer {
    tool_router: ToolRouter<Self>,
    store: Arc<Mutex<SupportStore>>,
}

impl SupportServer {
    fn new(store: SupportStore) -> Self {
        Self {
            tool_router: Self::tool_router(),
            store: Arc::new(Mutex::new(store)),
        }
    }

    async fn with_store<T, F>(&self, operation: F) -> Result<Json<T>, rmcp::ErrorData>
    where
        T: Send + 'static,
        F: FnOnce(&mut SupportStore) -> Result<T> + Send + 'static,
    {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = store
                .lock()
                .map_err(|_| anyhow!("support store недоступен"))?;
            operation(&mut store)
        })
        .await
        .map_err(|_| server_error(anyhow!("support store worker завершился с ошибкой")))?
        .map(Json)
        .map_err(server_error)
    }
}

fn server_error(error: anyhow::Error) -> rmcp::ErrorData {
    let message: String = error.to_string().chars().take(500).collect();
    let code = match message.as_str() {
        "тикет не найден" => "not_found",
        "тикет закрыт" => "closed",
        _ if message.contains("неверн") || message.contains("поле") => "invalid_input",
        _ => "store_unavailable",
    };
    rmcp::ErrorData::invalid_params("support operation failed", Some(json!({"code": code})))
}

#[tool_router(router = tool_router)]
impl SupportServer {
    #[tool(
        name = "create_ticket",
        description = "Создаёт пользователя, открытый тикет и первое сообщение",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn create_ticket(
        &self,
        Parameters(request): Parameters<CreateTicketRequest>,
    ) -> Result<Json<CreatedTicket>, rmcp::ErrorData> {
        self.with_store(move |store| store.create_ticket(&request))
            .await
    }

    #[tool(
        name = "get_ticket",
        description = "Возвращает карточку одного тикета и безопасные поля пользователя",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn get_ticket(
        &self,
        Parameters(request): Parameters<TicketIdRequest>,
    ) -> Result<Json<SupportTicket>, rmcp::ErrorData> {
        self.with_store(move |store| store.get_ticket(&request.ticket_id))
            .await
    }

    #[tool(
        name = "list_ticket_messages",
        description = "Возвращает ограниченную историю одного тикета",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn list_ticket_messages(
        &self,
        Parameters(request): Parameters<ListMessagesRequest>,
    ) -> Result<Json<TicketMessages>, rmcp::ErrorData> {
        self.with_store(move |store| store.list_messages(&request))
            .await
    }

    #[tool(
        name = "add_ticket_message",
        description = "Добавляет сообщение пользователя или проверенный ответ ассистента",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn add_ticket_message(
        &self,
        Parameters(request): Parameters<AddMessageRequest>,
    ) -> Result<Json<SupportMessage>, rmcp::ErrorData> {
        self.with_store(move |store| store.add_message(&request))
            .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SupportServer {}

async fn serve_support_mcp(
    listener: TcpListener,
    store: SupportStore,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let factory = SupportServer::new(store);
    let service = StreamableHttpService::new(
        move || Ok(factory.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = shutdown.await;
        })
        .await
        .context("support MCP-сервер завершился с ошибкой")
}

pub(crate) struct SupportMcpRuntime {
    client: SupportMcpClient,
    router: Arc<McpRouter>,
    shutdown: Option<oneshot::Sender<()>>,
    server_task: JoinHandle<Result<()>>,
}

impl SupportMcpRuntime {
    pub(crate) async fn start(db_path: PathBuf) -> Result<Self> {
        let store = tokio::task::spawn_blocking(move || SupportStore::open(&db_path))
            .await
            .context("задача открытия support-базы аварийно завершилась")??;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let url = format!("http://{}/mcp", listener.local_addr()?);
        let (tx, rx) = oneshot::channel();
        let server_task = tokio::spawn(serve_support_mcp(listener, store, rx));
        let router = Arc::new(
            McpRouter::connect(&[McpServerConfig {
                id: SUPPORT_SERVER_ID.into(),
                url,
                disabled_tools: vec![],
            }])
            .await,
        );
        anyhow::ensure!(
            router.warnings().is_empty(),
            "support MCP handshake: {}",
            router.warnings().join("; ")
        );
        let names = router
            .definitions()
            .into_iter()
            .map(|d| d.name)
            .collect::<std::collections::BTreeSet<_>>();
        let expected = [
            "create_ticket",
            "get_ticket",
            "list_ticket_messages",
            "add_ticket_message",
        ]
        .into_iter()
        .map(|n| format!("{SUPPORT_SERVER_ID}__{n}"))
        .collect();
        anyhow::ensure!(
            names == expected,
            "support MCP объявил неожиданные инструменты"
        );
        let client = SupportMcpClient {
            executor: router.clone(),
        };
        Ok(Self {
            client,
            router,
            shutdown: Some(tx),
            server_task,
        })
    }

    pub(crate) fn client(&self) -> SupportMcpClient {
        self.client.clone()
    }

    pub(crate) async fn shutdown(mut self) -> Result<()> {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let router = Arc::try_unwrap(self.router).ok();
        finish_support_shutdown(router, self.server_task).await
    }
}

async fn finish_support_shutdown(
    mut router: Option<McpRouter>,
    mut server_task: JoinHandle<Result<()>>,
) -> Result<()> {
    let graceful = async {
        if let Some(router) = router.take() {
            router.close().await;
        }
        (&mut server_task)
            .await
            .context("задача support MCP аварийно завершилась")??;
        Ok(())
    };

    if let Ok(result) = tokio::time::timeout(SHUTDOWN_TIMEOUT, graceful).await {
        return result;
    }

    server_task.abort();
    let _ = server_task.await;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct SupportMcpClient {
    executor: SharedToolExecutor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SupportMcpErrorKind {
    Rejected,
    InvalidInput,
    NotFound,
    Closed,
    Unavailable,
    InvalidResponse,
}

#[derive(Debug)]
struct SupportMcpClientError(SupportMcpErrorKind);

impl std::fmt::Display for SupportMcpClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self.0 {
            SupportMcpErrorKind::Rejected => "support MCP: операция отклонена",
            SupportMcpErrorKind::InvalidInput => "support MCP: неверные аргументы",
            SupportMcpErrorKind::NotFound => "support MCP: тикет не найден",
            SupportMcpErrorKind::Closed => "support MCP: тикет закрыт",
            SupportMcpErrorKind::Unavailable => "support MCP недоступен",
            SupportMcpErrorKind::InvalidResponse => "support MCP вернул неверный JSON",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SupportMcpClientError {}

pub(crate) fn support_mcp_error_kind(error: &anyhow::Error) -> SupportMcpErrorKind {
    error
        .downcast_ref::<SupportMcpClientError>()
        .map(|error| error.0)
        .unwrap_or(SupportMcpErrorKind::Unavailable)
}

impl SupportMcpClient {
    async fn call<T: DeserializeOwned>(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<T> {
        let result = self
            .executor
            .execute(&ToolCall {
                id: uuid::Uuid::new_v4().to_string(),
                name: format!("support__{name}"),
                arguments,
            })
            .await
            .map_err(|error| {
                let server_code = error
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<rmcp::ServiceError>())
                    .and_then(|error| match error {
                        rmcp::ServiceError::McpError(data) => Some(data),
                        _ => None,
                    })
                    .and_then(|data| data.data.as_ref())
                    .and_then(|data| data.get("code"))
                    .and_then(serde_json::Value::as_str);
                let kind = match server_code {
                    Some("invalid_input") => SupportMcpErrorKind::InvalidInput,
                    Some("not_found") => SupportMcpErrorKind::NotFound,
                    Some("closed") => SupportMcpErrorKind::Closed,
                    Some("store_unavailable") => SupportMcpErrorKind::Unavailable,
                    Some(_) => SupportMcpErrorKind::Rejected,
                    None => SupportMcpErrorKind::Unavailable,
                };
                SupportMcpClientError(kind)
            })?;
        if result.is_error {
            return Err(SupportMcpClientError(SupportMcpErrorKind::Rejected).into());
        }
        serde_json::from_value(result.content)
            .map_err(|_| SupportMcpClientError(SupportMcpErrorKind::InvalidResponse).into())
    }
    pub(crate) async fn create_ticket(
        &self,
        request: &CreateTicketRequest,
    ) -> Result<CreatedTicket> {
        self.call("create_ticket", serde_json::to_value(request)?)
            .await
    }
    pub(crate) async fn get_ticket(&self, ticket_id: &str) -> Result<SupportTicket> {
        self.call("get_ticket", json!({"ticket_id":ticket_id}))
            .await
    }
    pub(crate) async fn list_messages(&self, ticket_id: &str) -> Result<TicketMessages> {
        self.call("list_ticket_messages", json!({"ticket_id":ticket_id,"limit":MAX_TICKET_MESSAGES,"char_budget":MAX_TICKET_CONTEXT_CHARS})).await
    }
    pub(crate) async fn add_message(&self, request: &AddMessageRequest) -> Result<SupportMessage> {
        self.call("add_ticket_message", serde_json::to_value(request)?)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ToolFuture, ToolOutcome, ToolResult};

    struct StaticExecutor {
        result: ToolResult,
    }

    impl ToolExecutor for StaticExecutor {
        fn definitions(&self) -> Vec<crate::ToolDefinition> {
            vec![]
        }

        fn execute<'a>(&'a self, _call: &'a ToolCall) -> ToolFuture<'a> {
            let result = self.result.clone();
            Box::pin(async move { Ok(result) })
        }
    }

    fn request(email: &str) -> CreateTicketRequest {
        CreateTicketRequest {
            name: "Анна".into(),
            email: email.into(),
            subject: "Вход".into(),
            description: "Ошибка 401".into(),
        }
    }

    #[test]
    fn store_persists_and_isolates_tickets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("support.db");
        let mut store = SupportStore::open(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let first = store.create_ticket(&request("a@example.com")).unwrap();
        let second = store.create_ticket(&request("a@example.com")).unwrap();
        assert_eq!(first.ticket.user.id, second.ticket.user.id);
        assert_ne!(first.ticket.id, second.ticket.id);
        store
            .add_message(&AddMessageRequest {
                ticket_id: first.ticket.id.clone(),
                role: "assistant".into(),
                content: "Ответ".into(),
            })
            .unwrap();
        assert_eq!(
            store
                .list_messages(&ListMessagesRequest {
                    ticket_id: first.ticket.id.clone(),
                    limit: 40,
                    char_budget: 4000
                })
                .unwrap()
                .messages
                .len(),
            2
        );
        assert_eq!(
            store
                .list_messages(&ListMessagesRequest {
                    ticket_id: second.ticket.id.clone(),
                    limit: 40,
                    char_budget: 4000
                })
                .unwrap()
                .messages
                .len(),
            1
        );
        drop(store);
        assert_eq!(
            SupportStore::open(&path)
                .unwrap()
                .get_ticket(&first.ticket.id)
                .unwrap()
                .id,
            first.ticket.id
        );
    }

    #[test]
    fn store_is_transactional_rejects_closed_tickets_and_bounds_unicode_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("support.db");
        let mut store = SupportStore::open(&path).unwrap();
        let mut invalid = request("invalid@example.com");
        invalid.description.clear();
        assert!(store.create_ticket(&invalid).is_err());
        let users: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .unwrap();
        assert_eq!(users, 0);

        let created = store
            .create_ticket(&request("unicode@example.com"))
            .unwrap();
        for content in ["ёж", "два", "три"] {
            store
                .add_message(&AddMessageRequest {
                    ticket_id: created.ticket.id.clone(),
                    role: "user".into(),
                    content: content.into(),
                })
                .unwrap();
        }
        let bounded = store
            .list_messages(&ListMessagesRequest {
                ticket_id: created.ticket.id.clone(),
                limit: 2,
                char_budget: 6,
            })
            .unwrap();
        assert!(bounded.truncated);
        assert_eq!(
            bounded
                .messages
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["два", "три"]
        );

        store
            .connection
            .execute(
                "UPDATE tickets SET status='closed' WHERE id=?1",
                [&created.ticket.id],
            )
            .unwrap();
        assert!(store
            .add_message(&AddMessageRequest {
                ticket_id: created.ticket.id,
                role: "user".into(),
                content: "ещё".into(),
            })
            .is_err());
        assert!(store.get_ticket("T-missing").is_err());
    }

    #[test]
    fn support_tools_publish_expected_schemas_and_annotations() {
        let dir = tempfile::tempdir().unwrap();
        let server = SupportServer::new(SupportStore::open(&dir.path().join("db")).unwrap());
        assert_eq!(
            server
                .tool_router
                .map
                .keys()
                .map(|name| name.as_ref())
                .collect::<std::collections::BTreeSet<_>>(),
            [
                "add_ticket_message",
                "create_ticket",
                "get_ticket",
                "list_ticket_messages"
            ]
            .into_iter()
            .collect()
        );
        for name in ["get_ticket", "list_ticket_messages"] {
            let annotations = server.tool_router.map[name]
                .attr
                .annotations
                .as_ref()
                .unwrap();
            assert_eq!(annotations.read_only_hint, Some(true));
            assert_eq!(annotations.destructive_hint, Some(false));
        }
        for name in ["create_ticket", "add_ticket_message"] {
            let tool = &server.tool_router.map[name];
            let annotations = tool.attr.annotations.as_ref().unwrap();
            assert_eq!(annotations.read_only_hint, Some(false));
            assert_eq!(annotations.destructive_hint, Some(false));
            assert_eq!(annotations.idempotent_hint, Some(false));
            assert_eq!(tool.attr.input_schema["type"], "object");
        }
    }

    #[tokio::test]
    async fn support_tool_handlers_reject_invalid_arguments_without_cross_ticket_data() {
        let dir = tempfile::tempdir().unwrap();
        let server = SupportServer::new(SupportStore::open(&dir.path().join("db")).unwrap());
        let first = server
            .create_ticket(Parameters(request("first@example.test")))
            .await
            .unwrap()
            .0;
        let second = server
            .create_ticket(Parameters(request("second@example.test")))
            .await
            .unwrap()
            .0;
        assert!(server
            .create_ticket(Parameters(request("invalid")))
            .await
            .is_err());
        assert!(server
            .get_ticket(Parameters(TicketIdRequest {
                ticket_id: "T-missing".into()
            }))
            .await
            .is_err());
        assert!(server
            .list_ticket_messages(Parameters(ListMessagesRequest {
                ticket_id: first.ticket.id.clone(),
                limit: 0,
                char_budget: 100,
            }))
            .await
            .is_err());
        assert!(server
            .add_ticket_message(Parameters(AddMessageRequest {
                ticket_id: first.ticket.id.clone(),
                role: "system".into(),
                content: "неверная роль".into(),
            }))
            .await
            .is_err());
        let first_messages = server
            .list_ticket_messages(Parameters(ListMessagesRequest {
                ticket_id: first.ticket.id,
                limit: 40,
                char_budget: 4000,
            }))
            .await
            .unwrap()
            .0;
        assert!(first_messages
            .messages
            .iter()
            .all(|message| message.ticket_id != second.ticket.id));
    }

    #[tokio::test]
    async fn typed_client_sanitizes_mcp_errors_and_rejects_invalid_json() {
        let failed = SupportMcpClient {
            executor: Arc::new(StaticExecutor {
                result: ToolResult {
                    call_id: "one".into(),
                    content: json!({"error":"secret ticket payload"}),
                    is_error: true,
                    outcome: ToolOutcome::Failed,
                },
            }),
        };
        let error = failed
            .get_ticket("T-sensitive")
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(error, "support MCP: операция отклонена");
        assert_eq!(
            support_mcp_error_kind(&failed.get_ticket("T-any").await.unwrap_err()),
            SupportMcpErrorKind::Rejected
        );
        assert!(!error.contains("secret"));
        assert!(!error.contains("T-sensitive"));

        let malformed = SupportMcpClient {
            executor: Arc::new(StaticExecutor {
                result: ToolResult {
                    call_id: "two".into(),
                    content: json!({"unexpected":true}),
                    is_error: false,
                    outcome: ToolOutcome::Success,
                },
            }),
        };
        let error = malformed.get_ticket("T-one").await.unwrap_err();
        assert_eq!(
            support_mcp_error_kind(&error),
            SupportMcpErrorKind::InvalidResponse
        );
    }

    #[test]
    fn store_rejects_newer_schema_and_invalid_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.db");
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);
        assert!(SupportStore::open(&path).is_err());
        let mut store = SupportStore::open(&dir.path().join("valid.db")).unwrap();
        let created = store.create_ticket(&request("b@example.com")).unwrap();
        assert!(store
            .add_message(&AddMessageRequest {
                ticket_id: created.ticket.id,
                role: "system".into(),
                content: "x".into()
            })
            .is_err());
    }

    #[test]
    fn ticket_validation_rejects_invalid_fields_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SupportStore::open(&dir.path().join("validations.db")).unwrap();
        let mut cases = Vec::new();
        let mut empty_name = request("ok@example.test");
        empty_name.name = " ".into();
        cases.push(empty_name);
        let mut invalid_email = request("not-an-email");
        invalid_email.name = "Имя".into();
        cases.push(invalid_email);
        let mut long_subject = request("ok@example.test");
        long_subject.subject = "я".repeat(201);
        cases.push(long_subject);
        let mut long_description = request("ok@example.test");
        long_description.description = "я".repeat(8001);
        cases.push(long_description);
        for invalid in cases {
            assert!(store.create_ticket(&invalid).is_err());
        }
        let count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM tickets", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn runtime_exposes_real_mcp_tools() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = SupportMcpRuntime::start(dir.path().join("support.db"))
            .await
            .unwrap();
        let client = runtime.client();
        let created = client
            .create_ticket(&request("mcp@example.com"))
            .await
            .unwrap();
        assert_eq!(
            client.get_ticket(&created.ticket.id).await.unwrap().id,
            created.ticket.id
        );
        let missing = client.get_ticket("T-missing").await.unwrap_err();
        assert_eq!(
            support_mcp_error_kind(&missing),
            SupportMcpErrorKind::NotFound
        );
        runtime.shutdown().await.unwrap();
    }
}
