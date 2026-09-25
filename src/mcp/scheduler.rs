use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, FixedOffset, NaiveTime, TimeZone, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_TIMEZONE: &str = "Europe/Moscow";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn daily_schedule_rejects_unknown_timezone() {
        let now = Utc::now();
        let schedule = ScheduleSpec::Daily {
            time: "08:00".into(),
            timezone: "UTC".into(),
        };
        assert!(validate_schedule(&schedule, now).is_err());
    }
}
