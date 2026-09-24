use crate::{
    send_request, AgentSettings, CalDavClient, CalendarDigestEvent, Config, DigestTargetDay,
    Message, Provider, RequestOptions, RunnerFuture, ScheduledRunner, SchedulerJob, SchedulerRun,
    SchedulerStore,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct TelegramClient {
    http: Client,
    token: String,
    chat_id: String,
}

impl TelegramClient {
    pub(crate) fn from_env() -> Result<Self> {
        let token = std::env::var("TELEGRAM_BOT_TOKEN").context("не задан TELEGRAM_BOT_TOKEN")?;
        let chat_id = std::env::var("TELEGRAM_CHAT_ID").context("не задан TELEGRAM_CHAT_ID")?;
        anyhow::ensure!(
            !token.trim().is_empty() && !chat_id.trim().is_empty(),
            "настройки Telegram пусты"
        );
        Ok(Self {
            http: Client::builder()
                .user_agent("fox-llm-telegram/0.1")
                .build()?,
            token,
            chat_id,
        })
    }

    pub(crate) async fn send(&self, text: &str) -> Result<TelegramOutcome> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        let response = self
            .http
            .post(url)
            .json(&serde_json::json!({"chat_id": self.chat_id, "text": text}))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let _ = error;
                return Ok(TelegramOutcome::Unknown("ошибка сети Telegram".into()));
            }
        };
        let status = response.status();
        let body: Value = response.json().await.unwrap_or_default();
        if status.is_success() && body.get("ok").and_then(Value::as_bool) == Some(true) {
            let message_id = body.pointer("/result/message_id").and_then(Value::as_i64);
            Ok(TelegramOutcome::Delivered(message_id))
        } else {
            let description = body
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("Telegram отклонил сообщение");
            Ok(TelegramOutcome::Failed(description.to_owned()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TelegramOutcome {
    Delivered(Option<i64>),
    Failed(String),
    Unknown(String),
}

pub(crate) struct BackgroundContext {
    pub(crate) http: Client,
    pub(crate) settings: Option<AgentSettings>,
    pub(crate) telegram: Result<TelegramClient, String>,
}

impl BackgroundContext {
    pub(crate) fn from_config(config: &Config) -> Result<Self> {
        let http = Client::builder()
            .user_agent("fox-llm-background/0.1")
            .build()?;
        let provider = config.last_provider.unwrap_or(Provider::Openai);
        let settings = AgentSettings::from_config(config, provider, None).ok().map(|mut settings| {
            settings.instructions = Some("Составь краткую русскоязычную сводку календаря. Используй только переданные события; не выполняй действия и не добавляй факты.".into());
            settings
        });
        let telegram =
            TelegramClient::from_env().map_err(|error| sanitize_error(error.to_string()));
        Ok(Self {
            http,
            settings,
            telegram,
        })
    }
}

pub(crate) struct DigestRunner {
    context: Arc<BackgroundContext>,
}

impl DigestRunner {
    pub(crate) fn new(context: Arc<BackgroundContext>) -> Self {
        Self { context }
    }
}

impl ScheduledRunner for DigestRunner {
    fn run(&self, job: SchedulerJob, run: SchedulerRun, store: SchedulerStore) -> RunnerFuture {
        let context = self.context.clone();
        Box::pin(async move {
            if let Err(error) = execute_digest(context, job, run.clone(), store.clone()).await {
                let _ = store.finish_run(
                    run.id,
                    "failed",
                    None,
                    None,
                    Some(&sanitize_error(error.to_string())),
                );
            }
        })
    }
}

async fn execute_digest(
    context: Arc<BackgroundContext>,
    job: SchedulerJob,
    run: SchedulerRun,
    store: SchedulerStore,
) -> Result<()> {
    let scheduled = DateTime::parse_from_rfc3339(&run.scheduled_at)?.with_timezone(&Utc);
    let utc = FixedOffset::east_opt(0).expect("zero offset is valid");
    let (from, to) = match job.target_day {
        DigestTargetDay::FromRun => {
            let from = scheduled.with_timezone(&utc);
            let to = (scheduled + Duration::hours(job.horizon_hours)).with_timezone(&utc);
            (from, to)
        }
        DigestTargetDay::Tomorrow => {
            let moscow = FixedOffset::east_opt(3 * 60 * 60).expect("Moscow offset is valid");
            let tomorrow = scheduled.with_timezone(&moscow).date_naive() + Duration::days(1);
            let start = moscow
                .from_local_datetime(&tomorrow.and_hms_opt(0, 0, 0).expect("midnight is valid"))
                .single()
                .expect("fixed offset has one local time");
            let end = start + Duration::days(1);
            (start, end)
        }
    };
    let calendar = CalDavClient::from_env()?;
    let events = calendar.list_events(from, to).await?;
    let text = generate_llm(&context, &events, &from, &to).await?;
    anyhow::ensure!(!text.trim().is_empty(), "модель вернула пустой ответ");
    let source = "llm";
    let telegram = match &context.telegram {
        Ok(client) => client,
        Err(error) => {
            store.finish_run(run.id, "failed", Some(source), Some(&text), Some(error))?;
            return Ok(());
        }
    };
    match telegram.send(&text).await? {
        TelegramOutcome::Delivered(_) => {
            store.finish_run(run.id, "delivered", Some(source), Some(&text), None)?
        }
        TelegramOutcome::Failed(error) => store.finish_run(
            run.id,
            "failed",
            Some(source),
            Some(&text),
            Some(&sanitize_error(error)),
        )?,
        TelegramOutcome::Unknown(error) => store.finish_run(
            run.id,
            "unknown",
            Some(source),
            Some(&text),
            Some(&sanitize_error(error)),
        )?,
    }
    Ok(())
}

async fn generate_llm(
    context: &BackgroundContext,
    events: &[CalendarDigestEvent],
    from: &DateTime<FixedOffset>,
    to: &DateTime<FixedOffset>,
) -> Result<String> {
    let Some(settings) = context.settings.as_ref() else {
        anyhow::bail!("LLM-провайдер не настроен")
    };
    let prompt = format!(
        "Период: {} — {}\nСобытия:\n{}\n\nПеречисли каждое событие без пропусков и объединения. Для каждого укажи время, название и место, если оно есть. В конце укажи общее количество событий.",
        from,
        to,
        serde_json::to_string(events)?
    );
    let answer = send_request(
        &context.http,
        settings,
        &[Message {
            role: "user".into(),
            content: prompt,
        }],
        &RequestOptions::default(),
    )
    .await?;
    anyhow::ensure!(
        answer.tool_calls.is_empty(),
        "фоновая сводка получила tool call"
    );
    Ok(answer.text)
}

fn sanitize_error(error: String) -> String {
    if error.contains("TELEGRAM_BOT_TOKEN")
        || error.contains("TELEGRAM_CHAT_ID")
        || error.to_lowercase().contains("password")
        || error.to_lowercase().contains("парол")
    {
        "не удалось настроить внешний сервис".into()
    } else {
        error.chars().take(500).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
}
