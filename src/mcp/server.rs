use anyhow::{anyhow, Context, Result};
use chrono::Utc;
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
use std::sync::Arc;
use tokio::net::TcpListener;

use super::calendar::{CalDavClient, CalendarEventRequest, CalendarEventResult};
use super::{MCP_SERVER_ADDR, MCP_SERVER_URL};
use crate::{
    DigestTargetDay, JobStatus, ScheduleSpec, SchedulerHistory, SchedulerJob, SchedulerRun,
    SchedulerRuntime, SchedulerStore, SCHEDULER_FILE,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct EchoRequest {
    /// Text to return unchanged.
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CreateDigestScheduleRequest {
    name: String,
    schedule: ScheduleSpec,
    #[serde(default = "default_horizon")]
    horizon_hours: i64,
    /// Для запроса «на завтра» укажите `tomorrow`; иначе период начинается с момента запуска.
    #[serde(default)]
    target_day: DigestTargetDay,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

fn default_horizon() -> i64 {
    crate::DEFAULT_HORIZON_HOURS
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct JobIdRequest {
    job_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct HistoryRequest {
    job_id: i64,
    #[serde(default = "default_history_limit")]
    limit: u32,
}

fn default_history_limit() -> u32 {
    20
}

#[derive(Clone)]
struct EchoServer {
    tool_router: ToolRouter<Self>,
    scheduler: Arc<SchedulerRuntime>,
}

impl EchoServer {
    fn new(scheduler: Arc<SchedulerRuntime>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            scheduler,
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

    #[tool(
        name = "create_calendar_digest_schedule",
        description = "Создаёт сохраняемую периодическую сводку календаря с доставкой в Telegram. Для формулировки «на завтра» обязательно передайте target_day=tomorrow.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn create_calendar_digest_schedule(
        &self,
        Parameters(request): Parameters<CreateDigestScheduleRequest>,
    ) -> Result<Json<SchedulerJob>, rmcp::ErrorData> {
        let job = self
            .scheduler
            .store
            .create_job(
                &request.name,
                &request.schedule,
                request.horizon_hours,
                request.target_day,
                request.provider.as_deref(),
                request.model.as_deref(),
                Utc::now(),
            )
            .map_err(server_error)?;
        self.scheduler.notify();
        Ok(Json(job))
    }

    #[tool(
        name = "list_calendar_digest_schedules",
        description = "Показывает активные и приостановленные расписания календарных сводок",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn list_calendar_digest_schedules(
        &self,
    ) -> Result<Json<Vec<SchedulerJob>>, rmcp::ErrorData> {
        self.scheduler
            .store
            .list_jobs()
            .map(Json)
            .map_err(server_error)
    }

    #[tool(
        name = "pause_calendar_digest_schedule",
        description = "Приостанавливает расписание календарной сводки",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn pause_calendar_digest_schedule(
        &self,
        Parameters(request): Parameters<JobIdRequest>,
    ) -> Result<Json<SchedulerJob>, rmcp::ErrorData> {
        self.scheduler
            .store
            .set_status(request.job_id, JobStatus::Paused)
            .map(Json)
            .map_err(server_error)
    }

    #[tool(
        name = "resume_calendar_digest_schedule",
        description = "Возобновляет расписание календарной сводки",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn resume_calendar_digest_schedule(
        &self,
        Parameters(request): Parameters<JobIdRequest>,
    ) -> Result<Json<SchedulerJob>, rmcp::ErrorData> {
        let job = self
            .scheduler
            .store
            .set_status(request.job_id, JobStatus::Active)
            .map_err(server_error)?;
        self.scheduler.notify();
        Ok(Json(job))
    }

    #[tool(
        name = "delete_calendar_digest_schedule",
        description = "Удаляет расписание календарной сводки, сохраняя историю запусков",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn delete_calendar_digest_schedule(
        &self,
        Parameters(request): Parameters<JobIdRequest>,
    ) -> Result<Json<SchedulerJob>, rmcp::ErrorData> {
        self.scheduler
            .store
            .set_status(request.job_id, JobStatus::Deleted)
            .map(Json)
            .map_err(server_error)
    }

    #[tool(
        name = "run_calendar_digest_now",
        description = "Запускает календарную сводку немедленно без изменения расписания",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn run_calendar_digest_now(
        &self,
        Parameters(request): Parameters<JobIdRequest>,
    ) -> Result<Json<SchedulerRun>, rmcp::ErrorData> {
        let run = self
            .scheduler
            .store
            .claim_manual(request.job_id, Utc::now())
            .map_err(server_error)?;
        let job = self
            .scheduler
            .store
            .list_jobs()
            .map_err(server_error)?
            .into_iter()
            .find(|job| job.id == request.job_id)
            .ok_or_else(|| server_error(anyhow!("задание не найдено")))?;
        self.scheduler.dispatch(job, run.clone());
        Ok(Json(run))
    }

    #[tool(
        name = "get_calendar_digest_history",
        description = "Возвращает последние запуски и агрегированную историю сводки",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn get_calendar_digest_history(
        &self,
        Parameters(request): Parameters<HistoryRequest>,
    ) -> Result<Json<SchedulerHistory>, rmcp::ErrorData> {
        self.scheduler
            .store
            .history(request.job_id, request.limit)
            .map(Json)
            .map_err(server_error)
    }
}

fn server_error(error: anyhow::Error) -> rmcp::ErrorData {
    eprintln!("MCP scheduler: {error:#}");
    rmcp::ErrorData::internal_error(error.to_string(), None)
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
    let store = SchedulerStore::open(std::env::current_dir()?.join(SCHEDULER_FILE))?;
    let config = crate::Config::load(&std::env::current_dir()?.join(crate::CONFIG_FILE))?;
    let context = Arc::new(crate::BackgroundContext::from_config(&config)?);
    let runner = Arc::new(crate::DigestRunner::new(context));
    let scheduler = Arc::new(SchedulerRuntime::new(store, runner));
    scheduler.store.recover_interrupted()?;
    scheduler.clone().start();
    let listener = TcpListener::bind(MCP_SERVER_ADDR)
        .await
        .with_context(|| format!("не удалось запустить MCP-сервер на {MCP_SERVER_ADDR}"))?;
    println!("MCP-сервер запущен: {MCP_SERVER_URL}");
    serve_mcp_listener_with_scheduler(listener, scheduler).await
}

#[allow(dead_code)]
pub(crate) async fn serve_mcp_listener(listener: TcpListener) -> Result<()> {
    let store = SchedulerStore::open(temp_scheduler_path()).map_err(|error| {
        eprintln!("test scheduler init failed: {error:#}");
        error
    })?;
    let context = Arc::new(crate::BackgroundContext::from_config(
        &crate::Config::default(),
    )?);
    let runner = Arc::new(crate::DigestRunner::new(context));
    serve_mcp_listener_with_scheduler(listener, Arc::new(SchedulerRuntime::new(store, runner)))
        .await
}

#[allow(dead_code)]
fn temp_scheduler_path() -> std::path::PathBuf {
    std::path::PathBuf::from("/private/tmp")
        .join(format!("fox-scheduler-test-{}.db", uuid::Uuid::new_v4()))
}

async fn serve_mcp_listener_with_scheduler(
    listener: TcpListener,
    scheduler: Arc<SchedulerRuntime>,
) -> Result<()> {
    let service = StreamableHttpService::new(
        move || Ok(EchoServer::new(scheduler.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    axum::serve(listener, router)
        .await
        .context("MCP-сервер завершился с ошибкой")
}
