use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, tower::StreamableHttpService,
        StreamableHttpServerConfig,
    },
    Json, ServerHandler,
};
use rusqlite::{params, Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{net::TcpListener, sync::Notify};

use super::{
    execute_pipeline, next_run, validate_pipeline_plan, validate_schedule, McpRouter, PipelinePlan,
    PreauthorizedPolicy, ScheduleSpec, SharedToolExecutor, ToolExecutor, ToolOutcome,
    PIPELINE_TOOL_NAME,
};
use crate::config::McpServerConfig;

const MAX_CAPTURED_OUTPUT: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GenericJobStatus {
    Active,
    Paused,
    Deleted,
}

impl GenericJobStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Deleted => "deleted",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "deleted" => Ok(Self::Deleted),
            _ => Err(anyhow!("неизвестный статус задания")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct GenericJob {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) schedule: ScheduleSpec,
    pub(crate) pipeline: PipelinePlan,
    pub(crate) authorization_hash: String,
    pub(crate) status: GenericJobStatus,
    pub(crate) next_run_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct GenericRun {
    pub(crate) id: i64,
    pub(crate) job_id: i64,
    pub(crate) trigger: String,
    pub(crate) scheduled_at: String,
    pub(crate) status: String,
    pub(crate) trace: Option<Value>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct GenericHistory {
    pub(crate) job: GenericJob,
    pub(crate) runs: Vec<GenericRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateJobRequest {
    name: String,
    schedule: ScheduleSpec,
    pipeline: PipelinePlan,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct JobIdRequest {
    job_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HistoryRequest {
    job_id: i64,
    #[serde(default = "history_limit")]
    limit: u32,
}

fn history_limit() -> u32 {
    20
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok(format!("{hash:016x}"))
}

fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("не удалось открыть базу orchestrator {}", path.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(connection)
}

#[derive(Debug, Clone)]
pub(crate) struct GenericSchedulerStore {
    path: Arc<PathBuf>,
}

impl GenericSchedulerStore {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = Self {
            path: Arc::new(path.as_ref().to_path_buf()),
        };
        store.with_connection(|connection| {
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS orchestration_jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                schedule_json TEXT NOT NULL,
                pipeline_json TEXT NOT NULL,
                authorization_hash TEXT NOT NULL,
                status TEXT NOT NULL,
                next_run_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS orchestration_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id INTEGER NOT NULL REFERENCES orchestration_jobs(id),
                trigger TEXT NOT NULL,
                scheduled_at TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                status TEXT NOT NULL,
                trace_json TEXT,
                error TEXT
            );
            CREATE TABLE IF NOT EXISTS orchestration_run_steps (
                run_id INTEGER NOT NULL REFERENCES orchestration_runs(id),
                ordinal INTEGER NOT NULL,
                step_id TEXT NOT NULL,
                server_id TEXT,
                native_tool TEXT NOT NULL,
                status TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                output_json TEXT,
                error TEXT,
                PRIMARY KEY(run_id, ordinal)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS one_running_orchestration_run_per_job
                ON orchestration_runs(job_id) WHERE status='running';",
            )?;
            Ok(())
        })?;
        Ok(store)
    }

    fn with_connection<T>(&self, operation: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        operation(&open_connection(&self.path)?)
    }

    pub(crate) fn create_job(
        &self,
        name: &str,
        schedule: &ScheduleSpec,
        pipeline: &PipelinePlan,
        now: DateTime<Utc>,
    ) -> Result<GenericJob> {
        anyhow::ensure!(!name.trim().is_empty(), "имя задания не должно быть пустым");
        validate_schedule(schedule, now)?;
        let hash = canonical_hash(&(schedule, pipeline))?;
        let next = next_run(schedule, now)?.map(|value| value.to_rfc3339());
        self.with_connection(|connection| {
            let now = now.to_rfc3339();
            connection.execute("INSERT INTO orchestration_jobs(name,schedule_json,pipeline_json,authorization_hash,status,next_run_at,created_at,updated_at) VALUES(?1,?2,?3,?4,'active',?5,?6,?6)", params![name.trim(), serde_json::to_string(schedule)?, serde_json::to_string(pipeline)?, hash, next, now])?;
            self.load_job(connection, connection.last_insert_rowid())
        })
    }

    fn load_job(&self, connection: &Connection, id: i64) -> Result<GenericJob> {
        connection.query_row("SELECT id,name,schedule_json,pipeline_json,authorization_hash,status,next_run_at FROM orchestration_jobs WHERE id=?1", [id], |row| {
            let schedule: String = row.get(2)?;
            let pipeline: String = row.get(3)?;
            let status: String = row.get(5)?;
            Ok((row.get(0)?, row.get(1)?, schedule, pipeline, row.get(4)?, status, row.get(6)?))
        }).optional()?.ok_or_else(|| anyhow!("задание #{id} не найдено")).and_then(|row| Ok(GenericJob {
            id: row.0, name: row.1, schedule: serde_json::from_str(&row.2)?, pipeline: serde_json::from_str(&row.3)?, authorization_hash: row.4, status: GenericJobStatus::parse(&row.5)?, next_run_at: row.6,
        }))
    }

    pub(crate) fn list_jobs(&self) -> Result<Vec<GenericJob>> {
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT id FROM orchestration_jobs WHERE status!='deleted' ORDER BY id")?;
            let ids = statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            ids.into_iter()
                .map(|id| self.load_job(connection, id))
                .collect()
        })
    }

    pub(crate) fn set_status(&self, id: i64, status: GenericJobStatus) -> Result<GenericJob> {
        self.with_connection(|connection| {
            let current = self.load_job(connection, id)?;
            anyhow::ensure!(
                current.status != GenericJobStatus::Deleted || status == GenericJobStatus::Deleted,
                "удалённое задание нельзя возобновить"
            );
            let changed = connection.execute(
                "UPDATE orchestration_jobs SET status=?1,updated_at=?2 WHERE id=?3",
                params![status.as_str(), Utc::now().to_rfc3339(), id],
            )?;
            anyhow::ensure!(changed == 1, "задание #{id} не найдено");
            self.load_job(connection, id)
        })
    }

    pub(crate) fn claim_manual(
        &self,
        id: i64,
        now: DateTime<Utc>,
    ) -> Result<(GenericJob, GenericRun)> {
        self.with_connection(|connection| {
            let job = self.load_job(connection, id)?;
            anyhow::ensure!(job.status != GenericJobStatus::Deleted, "задание удалено");
            let run = self.insert_run(connection, job.id, "manual", &now.to_rfc3339(), now)?;
            Ok((job, run))
        })
    }

    pub(crate) fn claim_due(&self, now: DateTime<Utc>) -> Result<Option<(GenericJob, GenericRun)>> {
        self.with_connection(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| {
                let candidate: Option<(i64, String)> = connection.query_row(
                    "SELECT j.id,j.next_run_at FROM orchestration_jobs j WHERE j.status='active' AND j.next_run_at IS NOT NULL AND j.next_run_at<=?1 AND NOT EXISTS(SELECT 1 FROM orchestration_runs r WHERE r.job_id=j.id AND r.status='running') ORDER BY j.next_run_at,j.id LIMIT 1",
                    [now.to_rfc3339()], |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional()?;
                let Some((id, scheduled_at)) = candidate else { return Ok(None) };
                let job = self.load_job(connection, id)?;
                let next = match job.schedule { ScheduleSpec::Once { .. } => None, _ => next_run(&job.schedule, now + Duration::seconds(1))?.map(|value| value.to_rfc3339()) };
                connection.execute("UPDATE orchestration_jobs SET next_run_at=?1,updated_at=?2 WHERE id=?3", params![next, now.to_rfc3339(), id])?;
                let run = self.insert_run(connection, id, "scheduled", &scheduled_at, now)?;
                Ok(Some((job, run)))
            })();
            if result.is_err() { let _ = connection.execute_batch("ROLLBACK"); } else { connection.execute_batch("COMMIT")?; }
            result
        })
    }

    fn insert_run(
        &self,
        connection: &Connection,
        job_id: i64,
        trigger: &str,
        scheduled_at: &str,
        now: DateTime<Utc>,
    ) -> Result<GenericRun> {
        connection.execute("INSERT INTO orchestration_runs(job_id,trigger,scheduled_at,started_at,status) VALUES(?1,?2,?3,?4,'running')", params![job_id, trigger, scheduled_at, now.to_rfc3339()])?;
        Ok(GenericRun {
            id: connection.last_insert_rowid(),
            job_id,
            trigger: trigger.into(),
            scheduled_at: scheduled_at.into(),
            status: "running".into(),
            trace: None,
            error: None,
        })
    }

    pub(crate) fn finish_run(
        &self,
        run: &GenericRun,
        plan: &PipelinePlan,
        result: &super::ToolResult,
    ) -> Result<()> {
        let status = match result.outcome {
            ToolOutcome::Success => "succeeded",
            ToolOutcome::Failed => "failed",
            ToolOutcome::Unknown => "unknown",
        };
        let mut safe_trace = result.content.clone();
        if let Some(steps) = safe_trace.get_mut("steps").and_then(Value::as_array_mut) {
            for (index, step) in steps.iter_mut().enumerate() {
                if let Some(object) = step.as_object_mut() {
                    object.remove("arguments");
                    if !plan
                        .steps
                        .get(index)
                        .is_some_and(|source| source.capture_output)
                    {
                        object.remove("output");
                    } else if object
                        .get("output")
                        .is_some_and(|output| output.to_string().len() > MAX_CAPTURED_OUTPUT)
                    {
                        object.insert("output".into(), json!({"truncated": true}));
                    }
                }
            }
        }
        self.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute("UPDATE orchestration_runs SET status=?1,trace_json=?2,error=?3,finished_at=?4 WHERE id=?5", params![status, serde_json::to_string(&safe_trace)?, safe_trace.get("error").and_then(Value::as_str), Utc::now().to_rfc3339(), run.id])?;
            if let Some(steps) = safe_trace.get("steps").and_then(Value::as_array) {
                for step in steps {
                    transaction.execute("INSERT INTO orchestration_run_steps(run_id,ordinal,step_id,server_id,native_tool,status,duration_ms,output_json,error) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![run.id, step["ordinal"].as_i64(), step["id"].as_str(), step["server_id"].as_str(), step["native_tool"].as_str(), step["status"].as_str(), step["duration_ms"].as_u64(), step.get("output").map(serde_json::to_string).transpose()?, step["error"].as_str()])?;
                }
            }
            transaction.commit()?;
            Ok(())
        })
    }

    pub(crate) fn recover_interrupted(&self) -> Result<usize> {
        self.with_connection(|connection| Ok(connection.execute("UPDATE orchestration_runs SET status='interrupted',finished_at=?1,error='процесс был перезапущен' WHERE status='running'", [Utc::now().to_rfc3339()])?))
    }

    pub(crate) fn history(&self, id: i64, limit: u32) -> Result<GenericHistory> {
        self.with_connection(|connection| {
            let job = self.load_job(connection, id)?;
            let mut statement = connection.prepare("SELECT id,job_id,trigger,scheduled_at,status,trace_json,error FROM orchestration_runs WHERE job_id=?1 ORDER BY id DESC LIMIT ?2")?;
            let rows = statement.query_map(params![id, limit.min(100)], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, Option<String>>(5)?, row.get::<_, Option<String>>(6)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let runs = rows.into_iter().map(|row| Ok(GenericRun { id: row.0, job_id: row.1, trigger: row.2, scheduled_at: row.3, status: row.4, trace: row.5.map(|value| serde_json::from_str(&value)).transpose()?, error: row.6 })).collect::<Result<Vec<_>>>()?;
            Ok(GenericHistory { job, runs })
        })
    }
}

#[derive(Clone)]
pub(crate) struct OrchestratorRuntime {
    store: GenericSchedulerStore,
    servers: Vec<McpServerConfig>,
    wake: Arc<Notify>,
}

impl OrchestratorRuntime {
    pub(crate) fn new(store: GenericSchedulerStore, servers: Vec<McpServerConfig>) -> Self {
        Self {
            store,
            servers,
            wake: Arc::new(Notify::new()),
        }
    }

    async fn validate_plan(&self, plan: &PipelinePlan) -> Result<()> {
        anyhow::ensure!(
            plan.steps
                .iter()
                .all(|step| !step.tool.starts_with("orchestrator__")
                    && step.tool != PIPELINE_TOOL_NAME),
            "pipeline задания не может вызывать orchestrator"
        );
        let router = McpRouter::connect(&self.servers).await;
        validate_pipeline_plan(plan, &router.definitions())
    }

    fn dispatch(&self, job: GenericJob, run: GenericRun) {
        let runtime = self.clone();
        tokio::spawn(async move {
            runtime.execute(job, run).await;
        });
    }

    async fn execute(&self, job: GenericJob, run: GenericRun) {
        let result = async {
            anyhow::ensure!(canonical_hash(&(&job.schedule, &job.pipeline))? == job.authorization_hash, "authorization hash задания не совпадает");
            let router = Arc::new(McpRouter::connect(&self.servers).await);
            let definitions = router.definitions();
            let executor: SharedToolExecutor = router;
            let runtime = json!({"run": {"id": run.id, "trigger": run.trigger, "scheduled_at": run.scheduled_at}});
            let mut policy = PreauthorizedPolicy;
            Ok::<_, anyhow::Error>(execute_pipeline(&format!("run-{}", run.id), &job.pipeline, &definitions, executor, &runtime, &mut policy, std::time::Duration::from_secs(15)).await)
        }.await.unwrap_or_else(|error| super::ToolResult { call_id: format!("run-{}", run.id), content: json!({"status":"failed","steps":[],"error":error.to_string()}), is_error: true, outcome: ToolOutcome::Failed });
        if let Err(error) = self.store.finish_run(&run, &job.pipeline, &result) {
            eprintln!(
                "orchestrator: не удалось завершить run #{}: {error:#}",
                run.id
            );
        }
    }

    pub(crate) fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            let _ = self.store.recover_interrupted();
            loop {
                match self.store.claim_due(Utc::now()) {
                    Ok(Some((job, run))) => self.dispatch(job, run),
                    Ok(None) => {}
                    Err(error) => eprintln!("orchestrator: ошибка запуска: {error:#}"),
                }
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => {},
                    _ = self.wake.notified() => {},
                }
            }
        });
    }
}

#[derive(Clone)]
struct OrchestratorServer {
    tool_router: ToolRouter<Self>,
    runtime: Arc<OrchestratorRuntime>,
}

impl OrchestratorServer {
    fn new(runtime: Arc<OrchestratorRuntime>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            runtime,
        }
    }
}

fn server_error(error: anyhow::Error) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(error.to_string(), None)
}

#[tool_router(router = tool_router)]
impl OrchestratorServer {
    #[tool(
        name = "create_job",
        description = "Создаёт подтверждённое периодическое MCP-pipeline задание",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn create_job(
        &self,
        Parameters(request): Parameters<CreateJobRequest>,
    ) -> Result<Json<GenericJob>, rmcp::ErrorData> {
        self.runtime
            .validate_plan(&request.pipeline)
            .await
            .map_err(server_error)?;
        let job = self
            .runtime
            .store
            .create_job(
                &request.name,
                &request.schedule,
                &request.pipeline,
                Utc::now(),
            )
            .map_err(server_error)?;
        self.runtime.wake.notify_one();
        Ok(Json(job))
    }

    #[tool(
        name = "list_jobs",
        description = "Показывает активные и приостановленные задания",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn list_jobs(&self) -> Result<Json<Vec<GenericJob>>, rmcp::ErrorData> {
        self.runtime
            .store
            .list_jobs()
            .map(Json)
            .map_err(server_error)
    }

    #[tool(
        name = "pause_job",
        description = "Приостанавливает задание",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    fn pause_job(
        &self,
        Parameters(request): Parameters<JobIdRequest>,
    ) -> Result<Json<GenericJob>, rmcp::ErrorData> {
        self.runtime
            .store
            .set_status(request.job_id, GenericJobStatus::Paused)
            .map(Json)
            .map_err(server_error)
    }

    #[tool(
        name = "resume_job",
        description = "Возобновляет задание",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    fn resume_job(
        &self,
        Parameters(request): Parameters<JobIdRequest>,
    ) -> Result<Json<GenericJob>, rmcp::ErrorData> {
        let job = self
            .runtime
            .store
            .set_status(request.job_id, GenericJobStatus::Active)
            .map_err(server_error)?;
        self.runtime.wake.notify_one();
        Ok(Json(job))
    }

    #[tool(
        name = "delete_job",
        description = "Удаляет задание, сохраняя историю",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    fn delete_job(
        &self,
        Parameters(request): Parameters<JobIdRequest>,
    ) -> Result<Json<GenericJob>, rmcp::ErrorData> {
        self.runtime
            .store
            .set_status(request.job_id, GenericJobStatus::Deleted)
            .map(Json)
            .map_err(server_error)
    }

    #[tool(
        name = "run_job_now",
        description = "Запускает задание немедленно без изменения расписания",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    fn run_job_now(
        &self,
        Parameters(request): Parameters<JobIdRequest>,
    ) -> Result<Json<GenericRun>, rmcp::ErrorData> {
        let (job, run) = self
            .runtime
            .store
            .claim_manual(request.job_id, Utc::now())
            .map_err(server_error)?;
        self.runtime.dispatch(job, run.clone());
        Ok(Json(run))
    }

    #[tool(
        name = "get_job_history",
        description = "Возвращает историю запусков задания",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn get_job_history(
        &self,
        Parameters(request): Parameters<HistoryRequest>,
    ) -> Result<Json<GenericHistory>, rmcp::ErrorData> {
        self.runtime
            .store
            .history(request.job_id, request.limit)
            .map(Json)
            .map_err(server_error)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for OrchestratorServer {
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
            "orchestrator",
            &tool,
            step_id.as_deref(),
            None,
            None,
        );
        let started = std::time::Instant::now();
        let call = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
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
            "orchestrator",
            &tool,
            step_id.as_deref(),
            Some(outcome),
            Some(started.elapsed().as_millis()),
        );
        result
    }
}

pub(crate) async fn run_orchestrator_server(
    address: SocketAddr,
    database: PathBuf,
    servers: Vec<McpServerConfig>,
) -> Result<()> {
    let store = GenericSchedulerStore::open(database)?;
    let runtime = Arc::new(OrchestratorRuntime::new(
        store,
        servers
            .into_iter()
            .filter(|server| server.id != "orchestrator")
            .collect(),
    ));
    runtime.clone().start();
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("не удалось запустить MCP-сервер orchestrator на {address}"))?;
    super::log_server_started(super::McpServerKind::Orchestrator, address);
    let service = StreamableHttpService::new(
        move || Ok(OrchestratorServer::new(runtime.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    axum::serve(listener, router)
        .await
        .context("MCP-сервер orchestrator завершился с ошибкой")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn sample_plan() -> PipelinePlan {
        PipelinePlan {
            name: "test".into(),
            steps: vec![super::super::PipelineStep {
                id: "one".into(),
                tool: "source__read".into(),
                arguments: json!({}),
                capture_output: false,
            }],
        }
    }

    struct ScheduledExecutor {
        mode: &'static str,
        calls: Mutex<Vec<super::super::ToolCall>>,
    }

    impl ToolExecutor for ScheduledExecutor {
        fn definitions(&self) -> Vec<super::super::ToolDefinition> {
            vec![
                super::super::ToolDefinition {
                    name: "calendar__list_events".into(),
                    description: None,
                    input_schema: json!({
                        "type": "object",
                        "required": ["relative_to", "target_day"],
                        "properties": {
                            "relative_to": {"type": "string"},
                            "target_day": {"const": "tomorrow"}
                        }
                    }),
                    read_only: true,
                    destructive: false,
                },
                super::super::ToolDefinition {
                    name: "ai__generate_text".into(),
                    description: None,
                    input_schema: json!({
                        "type": "object",
                        "required": ["instructions", "input"]
                    }),
                    read_only: true,
                    destructive: false,
                },
                super::super::ToolDefinition {
                    name: "telegram__send_message".into(),
                    description: None,
                    input_schema: json!({
                        "type": "object",
                        "required": ["text"],
                        "properties": {"text": {"type": "string"}}
                    }),
                    read_only: false,
                    destructive: false,
                },
            ]
        }

        fn execute<'a>(&'a self, call: &'a super::super::ToolCall) -> super::super::ToolFuture<'a> {
            self.calls.lock().unwrap().push(call.clone());
            Box::pin(async move {
                let (content, outcome) = match call.name.as_str() {
                    "calendar__list_events" if self.mode == "source_failed" => {
                        (json!({"error": "source failed"}), ToolOutcome::Failed)
                    }
                    "calendar__list_events" => {
                        (json!([{"title": "Standup"}]), ToolOutcome::Success)
                    }
                    "ai__generate_text" if self.mode == "ai_failed" => {
                        (json!({"error": "provider failed"}), ToolOutcome::Failed)
                    }
                    "ai__generate_text" => {
                        (json!({"text": "Завтра: Standup"}), ToolOutcome::Success)
                    }
                    "telegram__send_message" if self.mode == "delivery_unknown" => (
                        json!({"outcome": "unknown", "error": "network"}),
                        ToolOutcome::Unknown,
                    ),
                    "telegram__send_message" => (
                        json!({"outcome": "delivered", "message_id": 7}),
                        ToolOutcome::Success,
                    ),
                    _ => unreachable!(),
                };
                Ok(super::super::ToolResult {
                    call_id: call.id.clone(),
                    is_error: outcome != ToolOutcome::Success,
                    content,
                    outcome,
                })
            })
        }
    }

    fn digest_plan() -> PipelinePlan {
        PipelinePlan {
            name: "tomorrow-digest".into(),
            steps: vec![
                super::super::PipelineStep {
                    id: "calendar".into(),
                    tool: "calendar__list_events".into(),
                    arguments: json!({
                        "relative_to": {"$ref": "run.scheduled_at"},
                        "target_day": "tomorrow"
                    }),
                    capture_output: false,
                },
                super::super::PipelineStep {
                    id: "summary".into(),
                    tool: "ai__generate_text".into(),
                    arguments: json!({
                        "instructions": "Составь сводку",
                        "input": {"$ref": "calendar.output"}
                    }),
                    capture_output: true,
                },
                super::super::PipelineStep {
                    id: "delivery".into(),
                    tool: "telegram__send_message".into(),
                    arguments: json!({"text": {"$ref": "summary.output.text"}}),
                    capture_output: false,
                },
            ],
        }
    }

    #[test]
    fn store_persists_generic_job_and_manual_run_preserves_next_time() {
        let directory = tempfile::tempdir().unwrap();
        let store = GenericSchedulerStore::open(directory.path().join("orchestrator.db")).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-09-25T05:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let job = store
            .create_job(
                "test",
                &ScheduleSpec::Daily {
                    time: "09:00".into(),
                    timezone: "Europe/Moscow".into(),
                },
                &sample_plan(),
                now,
            )
            .unwrap();
        let next = job.next_run_at.clone();
        let (_, run) = store.claim_manual(job.id, now).unwrap();
        assert_eq!(run.trigger, "manual");
        assert_eq!(store.list_jobs().unwrap()[0].next_run_at, next);
    }

    #[test]
    fn store_recovers_interrupted_runs() {
        let directory = tempfile::tempdir().unwrap();
        let store = GenericSchedulerStore::open(directory.path().join("orchestrator.db")).unwrap();
        let now = Utc::now();
        let job = store
            .create_job(
                "test",
                &ScheduleSpec::Once {
                    run_at: (now + Duration::hours(1)).to_rfc3339(),
                },
                &sample_plan(),
                now,
            )
            .unwrap();
        store.claim_manual(job.id, now).unwrap();
        assert_eq!(store.recover_interrupted().unwrap(), 1);
        assert_eq!(
            store.history(job.id, 10).unwrap().runs[0].status,
            "interrupted"
        );
    }

    #[tokio::test]
    async fn scheduled_calendar_ai_telegram_flow_persists_order_and_stops_on_failure() {
        for mode in ["success", "source_failed", "ai_failed", "delivery_unknown"] {
            let directory = tempfile::tempdir().unwrap();
            let store =
                GenericSchedulerStore::open(directory.path().join("orchestrator.db")).unwrap();
            let now = DateTime::parse_from_rfc3339("2026-09-25T05:00:00Z")
                .unwrap()
                .with_timezone(&Utc);
            let due = now + Duration::seconds(1);
            let plan = digest_plan();
            let job = store
                .create_job(
                    "digest",
                    &ScheduleSpec::Once {
                        run_at: due.to_rfc3339(),
                    },
                    &plan,
                    now,
                )
                .unwrap();
            let (_, run) = store
                .claim_due(due + Duration::seconds(1))
                .unwrap()
                .unwrap();
            assert_eq!(run.trigger, "scheduled");
            let executor = Arc::new(ScheduledExecutor {
                mode,
                calls: Mutex::new(Vec::new()),
            });
            let definitions = executor.definitions();
            let runtime = json!({"run": {
                "id": run.id,
                "trigger": run.trigger,
                "scheduled_at": run.scheduled_at
            }});
            let mut policy = PreauthorizedPolicy;
            let result = execute_pipeline(
                "scheduled-test",
                &plan,
                &definitions,
                executor.clone(),
                &runtime,
                &mut policy,
                std::time::Duration::from_secs(1),
            )
            .await;
            store.finish_run(&run, &plan, &result).unwrap();
            let calls = executor.calls.lock().unwrap();
            assert_eq!(calls[0].arguments["relative_to"], json!(run.scheduled_at));
            assert_eq!(
                calls.len(),
                match mode {
                    "source_failed" => 1,
                    "ai_failed" => 2,
                    _ => 3,
                }
            );
            drop(calls);
            let history = store.history(job.id, 10).unwrap();
            assert_eq!(history.runs.len(), 1);
            assert_eq!(
                history.runs[0].status,
                match mode {
                    "success" => "succeeded",
                    "delivery_unknown" => "unknown",
                    _ => "failed",
                }
            );
            let steps = history.runs[0].trace.as_ref().unwrap()["steps"]
                .as_array()
                .unwrap();
            assert_eq!(steps[0]["ordinal"], 1);
            assert_eq!(steps[0]["server_id"], "calendar");
            if mode == "source_failed" {
                assert_eq!(steps[1]["status"], "skipped");
                assert_eq!(steps[2]["status"], "skipped");
            } else {
                assert_eq!(steps[1]["server_id"], "ai");
            }
            if mode == "ai_failed" {
                assert_eq!(steps[2]["status"], "skipped");
            } else if matches!(mode, "success" | "delivery_unknown") {
                assert_eq!(steps[2]["server_id"], "telegram");
                assert!(steps[0].get("output").is_none());
                assert!(steps[1].get("output").is_some());
            }
        }
    }

    #[test]
    fn captured_outputs_are_bounded_in_persisted_history() {
        let directory = tempfile::tempdir().unwrap();
        let store = GenericSchedulerStore::open(directory.path().join("orchestrator.db")).unwrap();
        let now = Utc::now();
        let mut plan = sample_plan();
        plan.steps[0].capture_output = true;
        let job = store
            .create_job(
                "capture",
                &ScheduleSpec::Once {
                    run_at: (now + Duration::hours(1)).to_rfc3339(),
                },
                &plan,
                now,
            )
            .unwrap();
        let (_, run) = store.claim_manual(job.id, now).unwrap();
        let result = super::super::ToolResult {
            call_id: "capture".into(),
            content: json!({
                "status": "succeeded",
                "steps": [{
                    "ordinal": 1,
                    "id": "one",
                    "server_id": "source",
                    "native_tool": "read",
                    "status": "succeeded",
                    "duration_ms": 1,
                    "output": "x".repeat(MAX_CAPTURED_OUTPUT + 1)
                }]
            }),
            is_error: false,
            outcome: ToolOutcome::Success,
        };
        store.finish_run(&run, &plan, &result).unwrap();
        let history = store.history(job.id, 1).unwrap();
        assert_eq!(
            history.runs[0].trace.as_ref().unwrap()["steps"][0]["output"],
            json!({"truncated": true})
        );
    }
}
