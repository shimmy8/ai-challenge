use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, FixedOffset, NaiveTime, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};
use tokio::sync::Notify;

pub(crate) const DEFAULT_TIMEZONE: &str = "Europe/Moscow";
pub(crate) const DEFAULT_HORIZON_HOURS: i64 = 24;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DigestTargetDay {
    #[default]
    FromRun,
    Tomorrow,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ScheduleSpec {
    Once {
        run_at: String,
    },
    Daily {
        time: String,
        #[serde(default = "default_timezone")]
        timezone: String,
    },
}

fn default_timezone() -> String {
    DEFAULT_TIMEZONE.to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) enum JobStatus {
    Active,
    Paused,
    Deleted,
}

impl JobStatus {
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
pub(crate) struct SchedulerJob {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) schedule: ScheduleSpec,
    pub(crate) horizon_hours: i64,
    #[serde(default)]
    pub(crate) target_day: DigestTargetDay,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) status: JobStatus,
    pub(crate) next_run_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct SchedulerRun {
    pub(crate) id: i64,
    pub(crate) job_id: i64,
    pub(crate) trigger: String,
    pub(crate) scheduled_at: String,
    pub(crate) status: String,
    pub(crate) text_source: Option<String>,
    pub(crate) delivered_text: Option<String>,
    pub(crate) error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct SchedulerHistory {
    pub(crate) job: SchedulerJob,
    pub(crate) runs: Vec<SchedulerRun>,
    pub(crate) delivered: i64,
    pub(crate) failed: i64,
    pub(crate) interrupted: i64,
    pub(crate) unknown: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct SchedulerStore {
    path: Arc<PathBuf>,
}

impl SchedulerStore {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let connection = open_connection(&path)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS scheduler_jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                schedule_json TEXT NOT NULL,
                horizon_hours INTEGER NOT NULL CHECK(horizon_hours BETWEEN 1 AND 168),
                target_day TEXT NOT NULL DEFAULT 'from_run',
                provider TEXT,
                model TEXT,
                status TEXT NOT NULL CHECK(status IN ('active','paused','deleted')),
                next_run_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );
            CREATE TABLE IF NOT EXISTS scheduler_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id INTEGER NOT NULL REFERENCES scheduler_jobs(id),
                trigger TEXT NOT NULL,
                scheduled_at TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                status TEXT NOT NULL,
                text_source TEXT,
                delivered_text TEXT,
                error_message TEXT
            );
            CREATE INDEX IF NOT EXISTS scheduler_jobs_due ON scheduler_jobs(status, next_run_at);
            CREATE INDEX IF NOT EXISTS scheduler_runs_job ON scheduler_runs(job_id, id DESC);",
        )?;
        let columns = connection
            .prepare("PRAGMA table_info(scheduler_jobs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|column| column == "target_day") {
            connection.execute(
                "ALTER TABLE scheduler_jobs ADD COLUMN target_day TEXT NOT NULL DEFAULT 'from_run'",
                [],
            )?;
        }
        Ok(Self {
            path: Arc::new(path),
        })
    }

    fn with_connection<T>(&self, operation: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let connection = open_connection(&self.path)?;
        operation(&connection)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_job(
        &self,
        name: &str,
        schedule: &ScheduleSpec,
        horizon_hours: i64,
        target_day: DigestTargetDay,
        provider: Option<&str>,
        model: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<SchedulerJob> {
        validate_schedule(schedule, now)?;
        anyhow::ensure!(
            (1..=168).contains(&horizon_hours),
            "горизонт сводки должен быть от 1 до 168 часов"
        );
        let next = next_run(schedule, now)?;
        self.with_connection(|connection| {
            let now_text = now.to_rfc3339();
            connection.execute(
                "INSERT INTO scheduler_jobs(name,schedule_json,horizon_hours,target_day,provider,model,status,next_run_at,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,'active',?7,?8,?8)",
                params![name.trim(), serde_json::to_string(schedule)?, horizon_hours, serde_json::to_string(&target_day)?.trim_matches('"'), provider, model, next.map(|value| value.to_rfc3339()), now_text],
            )?;
            self.load_job(connection, connection.last_insert_rowid())
        })
    }

    pub(crate) fn list_jobs(&self) -> Result<Vec<SchedulerJob>> {
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT id FROM scheduler_jobs WHERE status != 'deleted' ORDER BY id")?;
            let ids = statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids.into_iter()
                .map(|id| self.load_job(connection, id))
                .collect()
        })
    }

    pub(crate) fn load_job(&self, connection: &Connection, id: i64) -> Result<SchedulerJob> {
        connection.query_row("SELECT id,name,schedule_json,horizon_hours,target_day,provider,model,status,next_run_at FROM scheduler_jobs WHERE id=?1", [id], |row| {
            let schedule_json: String = row.get(2)?;
            let id: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let horizon_hours: i64 = row.get(3)?;
            let target_day: String = row.get(4)?;
            let provider: Option<String> = row.get(5)?;
            let model: Option<String> = row.get(6)?;
            let status: String = row.get(7)?;
            let next_run_at: Option<String> = row.get(8)?;
            Ok((id, name, schedule_json, horizon_hours, target_day, provider, model, status, next_run_at))
        }).with_context(|| format!("задание #{id} не найдено")).and_then(|(id,name,schedule_json,horizon_hours,target_day,provider,model,status,next_run_at)| {
            Ok(SchedulerJob { id, name, schedule: serde_json::from_str(&schedule_json)?, horizon_hours, target_day: serde_json::from_str(&format!("\"{target_day}\""))?, provider, model, status: JobStatus::parse(&status)?, next_run_at })
        })
    }

    pub(crate) fn set_status(&self, id: i64, status: JobStatus) -> Result<SchedulerJob> {
        self.with_connection(|connection| {
            let changed = connection.execute("UPDATE scheduler_jobs SET status=?1, deleted_at=CASE WHEN ?1='deleted' THEN CURRENT_TIMESTAMP ELSE deleted_at END, updated_at=CURRENT_TIMESTAMP WHERE id=?2", params![status.as_str(), id])?;
            anyhow::ensure!(changed == 1, "задание #{id} не найдено");
            self.load_job(connection, id)
        })
    }

    pub(crate) fn claim_due(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<(SchedulerJob, SchedulerRun)>> {
        self.with_connection(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| {
                let candidate: Option<(i64, String)> = connection.query_row("SELECT id,next_run_at FROM scheduler_jobs WHERE status='active' AND next_run_at IS NOT NULL AND next_run_at <= ?1 ORDER BY next_run_at,id LIMIT 1", [now.to_rfc3339()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
                let Some((job_id, scheduled_at)) = candidate else { return Ok(None) };
                let job = self.load_job(connection, job_id)?;
                let next = match job.schedule { ScheduleSpec::Once { .. } => None, _ => next_run(&job.schedule, now + Duration::seconds(1))? };
                connection.execute("UPDATE scheduler_jobs SET next_run_at=?1,updated_at=?2 WHERE id=?3 AND status='active'", params![next.map(|value| value.to_rfc3339()), now.to_rfc3339(), job_id])?;
                let changed = connection.execute("INSERT INTO scheduler_runs(job_id,trigger,scheduled_at,started_at,status) VALUES(?1,'scheduled',?2,?3,'running')", params![job_id, scheduled_at, now.to_rfc3339()])?;
                anyhow::ensure!(changed == 1, "не удалось создать запуск");
                let run_id = connection.last_insert_rowid();
                Ok(Some((job, SchedulerRun { id: run_id, job_id, trigger: "scheduled".into(), scheduled_at, status: "running".into(), text_source: None, delivered_text: None, error_message: None })))
            })();
            if result.is_err() { let _ = connection.execute_batch("ROLLBACK"); } else { connection.execute_batch("COMMIT")?; }
            result
        })
    }

    pub(crate) fn claim_manual(&self, id: i64, now: DateTime<Utc>) -> Result<SchedulerRun> {
        self.with_connection(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| {
                let status: Option<String> = connection.query_row("SELECT status FROM scheduler_jobs WHERE id=?1", [id], |row| row.get(0)).optional()?;
                anyhow::ensure!(matches!(status.as_deref(), Some("active") | Some("paused")), "задание #{id} не найдено или удалено");
                let _: SchedulerJob = self.load_job(connection, id)?;
                connection.execute("INSERT INTO scheduler_runs(job_id,trigger,scheduled_at,started_at,status) VALUES(?1,'manual',?2,?2,'running')", params![id, now.to_rfc3339()])?;
                let run_id = connection.last_insert_rowid();
                Ok(SchedulerRun { id: run_id, job_id: id, trigger: "manual".into(), scheduled_at: now.to_rfc3339(), status: "running".into(), text_source: None, delivered_text: None, error_message: None })
            })();
            if result.is_err() { let _ = connection.execute_batch("ROLLBACK"); } else { connection.execute_batch("COMMIT")?; }
            result
        })
    }

    pub(crate) fn finish_run(
        &self,
        run_id: i64,
        status: &str,
        text_source: Option<&str>,
        text: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        self.with_connection(|connection| {
            let changed = connection.execute("UPDATE scheduler_runs SET status=?1,text_source=?2,delivered_text=?3,error_message=?4,finished_at=CURRENT_TIMESTAMP WHERE id=?5", params![status, text_source, text, error, run_id])?;
            anyhow::ensure!(changed == 1, "запуск #{run_id} не найден");
            Ok(())
        })
    }

    pub(crate) fn recover_interrupted(&self) -> Result<usize> {
        self.with_connection(|connection| Ok(connection.execute("UPDATE scheduler_runs SET status='interrupted',finished_at=CURRENT_TIMESTAMP,error_message='процесс был перезапущен' WHERE status='running'", [])?))
    }

    pub(crate) fn history(&self, id: i64, limit: u32) -> Result<SchedulerHistory> {
        self.with_connection(|connection| {
            let job = self.load_job(connection, id)?;
            let mut statement = connection.prepare("SELECT id,job_id,trigger,scheduled_at,status,text_source,delivered_text,error_message FROM scheduler_runs WHERE job_id=?1 ORDER BY id DESC LIMIT ?2")?;
            let runs = statement.query_map(params![id, limit.min(100)], |row| Ok(SchedulerRun { id: row.get(0)?, job_id: row.get(1)?, trigger: row.get(2)?, scheduled_at: row.get(3)?, status: row.get(4)?, text_source: row.get(5)?, delivered_text: row.get(6)?, error_message: row.get(7)? }))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let counts = connection.query_row("SELECT SUM(status='delivered'),SUM(status='failed'),SUM(status='interrupted'),SUM(status='unknown') FROM scheduler_runs WHERE job_id=?1", [id], |row| Ok((row.get::<_, Option<i64>>(0)?.unwrap_or(0),row.get::<_, Option<i64>>(1)?.unwrap_or(0),row.get::<_, Option<i64>>(2)?.unwrap_or(0),row.get::<_, Option<i64>>(3)?.unwrap_or(0))))?;
            Ok(SchedulerHistory { job, runs, delivered: counts.0, failed: counts.1, interrupted: counts.2, unknown: counts.3 })
        })
    }
}

fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("не удалось открыть базу планировщика {}", path.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(connection)
}

pub(crate) fn validate_schedule(schedule: &ScheduleSpec, now: DateTime<Utc>) -> Result<()> {
    match schedule {
        ScheduleSpec::Once { run_at } => {
            let parsed = DateTime::parse_from_rfc3339(run_at)
                .context("одноразовое время должно быть RFC 3339 со смещением")?;
            anyhow::ensure!(
                parsed.with_timezone(&Utc) > now,
                "одноразовое время должно быть в будущем"
            );
        }
        ScheduleSpec::Daily { time, timezone } => {
            NaiveTime::parse_from_str(time, "%H:%M")
                .context("ежедневное время должно иметь формат HH:MM")?;
            anyhow::ensure!(
                timezone == DEFAULT_TIMEZONE,
                "поддерживается только Europe/Moscow"
            );
        }
    }
    Ok(())
}

pub(crate) fn next_run(
    schedule: &ScheduleSpec,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    match schedule {
        ScheduleSpec::Once { run_at } => Ok(Some(
            DateTime::parse_from_rfc3339(run_at)?.with_timezone(&Utc),
        )),
        ScheduleSpec::Daily { time, .. } => {
            let offset = FixedOffset::east_opt(3 * 3600)
                .ok_or_else(|| anyhow!("некорректная временная зона"))?;
            let local_now = now.with_timezone(&offset);
            let time = NaiveTime::parse_from_str(time, "%H:%M")?;
            let date = if local_now.time() < time {
                local_now.date_naive()
            } else {
                local_now.date_naive() + Duration::days(1)
            };
            Ok(Some(
                offset
                    .from_local_datetime(&date.and_time(time))
                    .single()
                    .ok_or_else(|| anyhow!("не удалось вычислить ежедневный запуск"))?
                    .with_timezone(&Utc),
            ))
        }
    }
}

pub(crate) type RunnerFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

pub(crate) trait ScheduledRunner: Send + Sync {
    fn run(&self, job: SchedulerJob, run: SchedulerRun, store: SchedulerStore) -> RunnerFuture;
}

pub(crate) struct SchedulerRuntime {
    pub(crate) store: SchedulerStore,
    pub(crate) wake: Arc<Notify>,
    runner: Arc<dyn ScheduledRunner>,
}

impl SchedulerRuntime {
    pub(crate) fn new(store: SchedulerStore, runner: Arc<dyn ScheduledRunner>) -> Self {
        Self {
            store,
            wake: Arc::new(Notify::new()),
            runner,
        }
    }
    pub(crate) fn notify(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn dispatch(&self, job: SchedulerJob, run: SchedulerRun) {
        let runner = self.runner.clone();
        let store = self.store.clone();
        tokio::spawn(runner.run(job, run, store));
    }

    pub(crate) fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            let _ = self.store.recover_interrupted();
            loop {
                match self.store.claim_due(Utc::now()) {
                    Ok(Some((job, run))) => self.dispatch(job, run),
                    Ok(None) => {}
                    Err(error) => eprintln!("scheduler: ошибка запуска: {error:#}"),
                }
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => {},
                    _ = self.wake.notified() => {},
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn daily_schedule_defaults_to_moscow_and_advances_to_next_day() {
        let now = DateTime::parse_from_rfc3339("2026-09-23T04:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let schedule: ScheduleSpec =
            serde_json::from_str(r#"{"type":"daily","time":"08:00"}"#).unwrap();
        assert!(validate_schedule(&schedule, now).is_ok());
        assert_eq!(
            next_run(&schedule, now).unwrap().unwrap().to_rfc3339(),
            "2026-09-23T05:00:00+00:00"
        );
    }

    #[test]
    fn store_claims_once_and_manual_does_not_change_plan() {
        let directory = tempdir().unwrap();
        let store = SchedulerStore::open(directory.path().join("scheduler.db")).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-09-23T04:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let job = store
            .create_job(
                "test",
                &ScheduleSpec::Once {
                    run_at: "2026-09-23T04:00:01Z".into(),
                },
                24,
                DigestTargetDay::FromRun,
                None,
                None,
                now,
            )
            .unwrap();
        let claimed = store
            .claim_due(now + Duration::seconds(2))
            .unwrap()
            .unwrap();
        assert_eq!(claimed.0.id, job.id);
        assert!(store
            .claim_due(now + Duration::seconds(3))
            .unwrap()
            .is_none());
        let manual = store
            .claim_manual(job.id, now + Duration::seconds(4))
            .unwrap();
        assert_eq!(manual.trigger, "manual");
        let history = store.history(job.id, 20).unwrap();
        assert_eq!(history.runs.len(), 2);
        assert!(history.runs.iter().all(|run| run.delivered_text.is_none()));
    }
}
