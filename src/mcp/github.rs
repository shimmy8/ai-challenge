use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Client, StatusCode,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration as StdDuration,
};

const GITHUB_API_URL: &str = "https://api.github.com";
const MAX_REQUESTS_UNAUTHENTICATED: usize = 48;
const MAX_REQUESTS_AUTHENTICATED: usize = 120;
const MAX_PAGES: usize = 20;
const PAGE_SIZE: usize = 100;
const STATS_RETRY_DELAYS_SECONDS: [u64; 3] = [1, 2, 4];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct GithubRepositoryRequest {
    /// Public repository in owner/name form.
    pub(crate) repository: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct GithubActivityRequest {
    /// Public repository in owner/name form.
    pub(crate) repository: String,
    /// Reporting window from 1 through 365 days.
    pub(crate) period_days: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct GithubRepositoryMetadata {
    pub(crate) repository: String,
    pub(crate) description: Option<String>,
    pub(crate) stars: u64,
    pub(crate) forks: u64,
    pub(crate) subscribers: u64,
    pub(crate) open_issues_and_pulls: u64,
    pub(crate) default_branch: String,
    pub(crate) created_at: String,
    pub(crate) pushed_at: Option<String>,
    pub(crate) languages: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct GithubProjectActivity {
    pub(crate) repository: String,
    pub(crate) period_days: u16,
    pub(crate) since: String,
    pub(crate) commits: Option<u64>,
    pub(crate) active_contributors: Option<u64>,
    pub(crate) issues_opened: Option<u64>,
    pub(crate) issues_closed: Option<u64>,
    pub(crate) pull_requests_opened: Option<u64>,
    pub(crate) pull_requests_merged: Option<u64>,
    pub(crate) merge_hours: Vec<f64>,
    pub(crate) workflow_runs: Option<u64>,
    pub(crate) workflow_successes: Option<u64>,
    pub(crate) release_times: Option<Vec<String>>,
    pub(crate) truncated: bool,
    #[serde(default)]
    pub(crate) incomplete_sources: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct CalculateGithubMetricsRequest {
    pub(crate) metadata: GithubRepositoryMetadata,
    pub(crate) activity: GithubProjectActivity,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetricKind {
    Source,
    Derived,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct MetricValue {
    pub(crate) value: Option<f64>,
    pub(crate) unit: String,
    pub(crate) kind: MetricKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct GithubMetrics {
    pub(crate) repository: String,
    pub(crate) period_days: u16,
    pub(crate) commits: MetricValue,
    pub(crate) active_contributors: MetricValue,
    pub(crate) issue_closure_rate: MetricValue,
    pub(crate) pull_request_merge_rate: MetricValue,
    pub(crate) median_merge_hours: MetricValue,
    pub(crate) ci_success_rate: MetricValue,
    pub(crate) release_cadence_days: MetricValue,
    pub(crate) warnings: Vec<String>,
}

struct PageResult {
    values: Vec<Value>,
    truncated: bool,
}

#[derive(Clone)]
pub(crate) struct GithubClient {
    http: Client,
    base_url: String,
    max_requests: usize,
    stats_retry_delays: Vec<StdDuration>,
}

impl GithubClient {
    pub(crate) fn public() -> Result<Self> {
        let token = env::var("GITHUB_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        Self::with_base_url_and_token(GITHUB_API_URL, token.as_deref())
    }

    #[cfg(test)]
    pub(crate) fn with_base_url(base_url: &str) -> Result<Self> {
        Self::with_base_url_and_token(base_url, None)
    }

    fn with_base_url_and_token(base_url: &str, token: Option<&str>) -> Result<Self> {
        let mut default_headers = HeaderMap::new();
        if let Some(token) = token.map(str::trim).filter(|token| !token.is_empty()) {
            let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
                .context("переменная GITHUB_TOKEN имеет некорректный формат")?;
            authorization.set_sensitive(true);
            default_headers.insert(AUTHORIZATION, authorization);
        }
        let http = Client::builder()
            .user_agent("fox-llm/0.1 github-report")
            .default_headers(default_headers)
            .timeout(StdDuration::from_secs(8))
            .build()
            .context("не удалось настроить GitHub-клиент")?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            max_requests: if token.is_some() {
                MAX_REQUESTS_AUTHENTICATED
            } else {
                MAX_REQUESTS_UNAUTHENTICATED
            },
            stats_retry_delays: STATS_RETRY_DELAYS_SECONDS
                .into_iter()
                .map(StdDuration::from_secs)
                .collect(),
        })
    }

    #[cfg(test)]
    fn with_request_limit(mut self, max_requests: usize) -> Self {
        self.max_requests = max_requests;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_stats_retry_delays(mut self, delays: Vec<StdDuration>) -> Self {
        self.stats_retry_delays = delays;
        self
    }

    pub(crate) async fn repository_metadata(
        &self,
        request: &GithubRepositoryRequest,
    ) -> Result<GithubRepositoryMetadata> {
        validate_repository(&request.repository)?;
        let budget = AtomicUsize::new(0);
        let repo_path = format!("/repos/{}", request.repository);
        let languages_path = format!("/repos/{}/languages", request.repository);
        let repo = self.request_json(&repo_path, &budget).await?;
        let languages = self.request_json(&languages_path, &budget).await?;
        Ok(GithubRepositoryMetadata {
            repository: request.repository.clone(),
            description: repo
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            stars: required_u64(&repo, "stargazers_count")?,
            forks: required_u64(&repo, "forks_count")?,
            subscribers: required_u64(&repo, "subscribers_count")?,
            open_issues_and_pulls: required_u64(&repo, "open_issues_count")?,
            default_branch: required_str(&repo, "default_branch")?.to_owned(),
            created_at: required_str(&repo, "created_at")?.to_owned(),
            pushed_at: repo
                .get("pushed_at")
                .and_then(Value::as_str)
                .map(str::to_owned),
            languages: serde_json::from_value(languages)
                .context("GitHub вернул некорректное распределение языков")?,
        })
    }

    pub(crate) async fn project_activity(
        &self,
        request: &GithubActivityRequest,
    ) -> Result<GithubProjectActivity> {
        validate_repository(&request.repository)?;
        if !(1..=365).contains(&request.period_days) {
            bail!("period_days должен быть от 1 до 365");
        }
        let since = Utc::now() - Duration::days(i64::from(request.period_days));
        let budget = AtomicUsize::new(0);
        let mut warnings = Vec::new();
        let mut truncated = false;
        let mut incomplete_sources = Vec::new();

        let commit_path = format!("/repos/{}/stats/commit_activity", request.repository);
        let contributor_path = format!("/repos/{}/stats/contributors", request.repository);
        let (commit_activity, contributor_activity) = tokio::join!(
            self.request_stats_json(&commit_path, &budget),
            self.request_stats_json(&contributor_path, &budget),
        );
        let commits = match commit_activity? {
            Some(activity) => activity.as_array().map(|weeks| {
                weeks
                    .iter()
                    .filter(|week| {
                        week.get("week")
                            .and_then(Value::as_i64)
                            .and_then(|value| DateTime::from_timestamp(value, 0))
                            .is_some_and(|date| date >= since)
                    })
                    .filter_map(|week| week.get("total").and_then(Value::as_u64))
                    .sum()
            }),
            None => {
                incomplete_sources.push("commits".into());
                warnings.push(
                    "commit statistics ещё формируется GitHub; commits недоступны, повторите запрос позже"
                        .into(),
                );
                None
            }
        };
        let active_contributors = match contributor_activity? {
            Some(activity) => activity.as_array().map(|contributors| {
                contributors
                    .iter()
                    .filter(|contributor| {
                        contributor
                            .get("weeks")
                            .and_then(Value::as_array)
                            .is_some_and(|weeks| {
                                weeks.iter().any(|week| {
                                    let recent = week
                                        .get("w")
                                        .and_then(Value::as_i64)
                                        .and_then(|value| DateTime::from_timestamp(value, 0))
                                        .is_some_and(|date| date >= since);
                                    let has_commits =
                                        week.get("c").and_then(Value::as_u64).unwrap_or(0) > 0;
                                    recent && has_commits
                                })
                            })
                    })
                    .count() as u64
            }),
            None => {
                let fallback_path = format!(
                    "/repos/{}/commits?since={}&per_page={PAGE_SIZE}",
                    request.repository,
                    since.to_rfc3339()
                );
                match self.fetch_pages(&fallback_path, None, None, &budget).await {
                    Ok(result) => {
                        let contributors = result
                            .values
                            .iter()
                            .filter_map(commit_author_identity)
                            .collect::<BTreeSet<_>>()
                            .len() as u64;
                        warnings.push(
                            "contributor statistics ещё формируется GitHub; active contributors рассчитаны по уникальным авторам коммитов"
                                .into(),
                        );
                        if result.truncated {
                            truncated = true;
                            incomplete_sources.push("contributors".into());
                            warnings.push(
                                "contributors усечены ограничением пагинации; значение является нижней оценкой"
                                    .into(),
                            );
                        }
                        Some(contributors)
                    }
                    Err(error) => {
                        incomplete_sources.push("contributors".into());
                        warnings.push(format!(
                            "contributor statistics ещё формируется GitHub, fallback по авторам коммитов недоступен: {error}"
                        ));
                        None
                    }
                }
            }
        };

        let issues_result = self
            .fetch_pages(
                &format!(
                    "/repos/{}/issues?state=all&since={}&per_page={PAGE_SIZE}",
                    request.repository,
                    since.to_rfc3339()
                ),
                None,
                None,
                &budget,
            )
            .await;
        let (issues, issues_available) = match issues_result {
            Ok(result) => {
                if result.truncated {
                    truncated = true;
                    incomplete_sources.push("issues".into());
                    warnings.push("issues усечены ограничением пагинации".into());
                }
                (result.values, true)
            }
            Err(error) => {
                incomplete_sources.push("issues".into());
                warnings.push(format!("issues недоступны: {error}"));
                (Vec::new(), false)
            }
        };
        let issue_items = issues
            .iter()
            .filter(|item| item.get("pull_request").is_none())
            .collect::<Vec<_>>();
        let issues_opened = issues_available.then(|| {
            issue_items
                .iter()
                .filter(|item| timestamp_at(item, "created_at").is_some_and(|date| date >= since))
                .count() as u64
        });
        let issues_closed = issues_available.then(|| {
            issue_items
                .iter()
                .filter(|item| timestamp_at(item, "closed_at").is_some_and(|date| date >= since))
                .count() as u64
        });

        let pulls_result = self
            .fetch_pages(
                &format!(
                    "/repos/{}/pulls?state=all&sort=updated&direction=desc&per_page={PAGE_SIZE}",
                    request.repository
                ),
                None,
                Some(("updated_at", since)),
                &budget,
            )
            .await;
        let (pulls, pulls_available) = match pulls_result {
            Ok(result) => {
                if result.truncated {
                    truncated = true;
                    incomplete_sources.push("pull_requests".into());
                    warnings.push("pull_requests усечены ограничением пагинации".into());
                }
                (result.values, true)
            }
            Err(error) => {
                incomplete_sources.push("pull_requests".into());
                warnings.push(format!("pull requests недоступны: {error}"));
                (Vec::new(), false)
            }
        };
        let relevant_pulls = pulls
            .iter()
            .filter(|item| timestamp_at(item, "created_at").is_some_and(|date| date >= since))
            .collect::<Vec<_>>();
        let pull_requests_opened = pulls_available.then_some(relevant_pulls.len() as u64);
        let merged = pulls
            .iter()
            .filter_map(|item| {
                let created = timestamp_at(item, "created_at")?;
                let merged = timestamp_at(item, "merged_at")?;
                (merged >= since).then_some((merged - created).num_seconds() as f64 / 3600.0)
            })
            .collect::<Vec<_>>();
        let pull_requests_merged = pulls_available.then_some(merged.len() as u64);

        let workflow_result = self
            .fetch_pages(
                &format!(
                    "/repos/{}/actions/runs?created=>={}&per_page={PAGE_SIZE}",
                    request.repository,
                    since.format("%Y-%m-%d")
                ),
                Some("workflow_runs"),
                None,
                &budget,
            )
            .await;
        let (workflow_values, workflows_available) = match workflow_result {
            Ok(result) => {
                if result.truncated {
                    truncated = true;
                    incomplete_sources.push("actions".into());
                    warnings.push("actions усечены ограничением пагинации".into());
                }
                (result.values, true)
            }
            Err(error) => {
                incomplete_sources.push("actions".into());
                warnings.push(format!("GitHub Actions недоступны: {error}"));
                (Vec::new(), false)
            }
        };
        let workflow_runs = workflows_available.then_some(workflow_values.len() as u64);
        let workflow_successes = workflows_available.then(|| {
            workflow_values
                .iter()
                .filter(|run| run.get("conclusion").and_then(Value::as_str) == Some("success"))
                .count() as u64
        });

        let release_result = self
            .fetch_pages(
                &format!(
                    "/repos/{}/releases?per_page={PAGE_SIZE}",
                    request.repository
                ),
                None,
                Some(("published_at", since)),
                &budget,
            )
            .await;
        let (release_values, releases_available) = match release_result {
            Ok(result) => {
                if result.truncated {
                    truncated = true;
                    incomplete_sources.push("releases".into());
                    warnings.push("releases усечены ограничением пагинации".into());
                }
                (result.values, true)
            }
            Err(error) => {
                incomplete_sources.push("releases".into());
                warnings.push(format!("releases недоступны: {error}"));
                (Vec::new(), false)
            }
        };
        let release_times = releases_available.then(|| {
            release_values
                .iter()
                .filter_map(|release| {
                    let published = timestamp_at(release, "published_at")?;
                    (published >= since).then(|| published.to_rfc3339())
                })
                .collect::<Vec<_>>()
        });
        incomplete_sources.sort();
        incomplete_sources.dedup();

        Ok(GithubProjectActivity {
            repository: request.repository.clone(),
            period_days: request.period_days,
            since: since.to_rfc3339(),
            commits,
            active_contributors,
            issues_opened,
            issues_closed,
            pull_requests_opened,
            pull_requests_merged,
            merge_hours: merged,
            workflow_runs,
            workflow_successes,
            release_times,
            truncated,
            incomplete_sources,
            warnings,
        })
    }

    async fn request_stats_json(&self, path: &str, budget: &AtomicUsize) -> Result<Option<Value>> {
        for attempt in 0..=self.stats_retry_delays.len() {
            match self.request_json_allow_pending(path, budget).await? {
                Some(value) => return Ok(Some(value)),
                None if attempt < self.stats_retry_delays.len() => {
                    tokio::time::sleep(self.stats_retry_delays[attempt]).await;
                }
                None => return Ok(None),
            }
        }
        unreachable!()
    }

    async fn fetch_pages(
        &self,
        path: &str,
        array_field: Option<&str>,
        cutoff: Option<(&str, DateTime<Utc>)>,
        budget: &AtomicUsize,
    ) -> Result<PageResult> {
        let mut all = Vec::new();
        for page in 1..=MAX_PAGES {
            let separator = if path.contains('?') { '&' } else { '?' };
            let value = self
                .request_json(&format!("{path}{separator}page={page}"), budget)
                .await?;
            let items = match array_field {
                Some(field) => value.get(field).and_then(Value::as_array),
                None => value.as_array(),
            }
            .ok_or_else(|| anyhow!("GitHub вернул неожиданный формат списка"))?;
            let reached_cutoff = cutoff.as_ref().is_some_and(|(field, cutoff)| {
                items.iter().any(|item| {
                    timestamp_at(item, field).is_some_and(|timestamp| timestamp < *cutoff)
                })
            });
            all.extend(items.iter().cloned());
            if items.len() < PAGE_SIZE || reached_cutoff {
                return Ok(PageResult {
                    values: all,
                    truncated: false,
                });
            }
        }
        Ok(PageResult {
            values: all,
            truncated: true,
        })
    }

    async fn request_json(&self, path: &str, budget: &AtomicUsize) -> Result<Value> {
        self.request_json_allow_pending(path, budget)
            .await?
            .ok_or_else(|| anyhow!("GitHub ещё формирует данные; повторите запрос позже"))
    }

    async fn request_json_allow_pending(
        &self,
        path: &str,
        budget: &AtomicUsize,
    ) -> Result<Option<Value>> {
        let used = budget.fetch_add(1, Ordering::Relaxed) + 1;
        if used > self.max_requests {
            bail!("превышен внутренний лимит запросов к GitHub");
        }
        let response = self
            .http
            .get(format!("{}{}", self.base_url, path))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|_| anyhow!("ошибка сети GitHub"))?;
        match response.status() {
            StatusCode::ACCEPTED => Ok(None),
            StatusCode::NOT_FOUND => bail!("публичный GitHub-репозиторий не найден"),
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
                bail!("GitHub API отклонил запрос или исчерпан rate limit")
            }
            status if !status.is_success() => {
                bail!("GitHub API вернул ошибку HTTP {}", status.as_u16())
            }
            _ => response
                .json::<Value>()
                .await
                .map(Some)
                .map_err(|_| anyhow!("GitHub вернул некорректный JSON")),
        }
    }
}

pub(crate) fn calculate_github_metrics(
    request: &CalculateGithubMetricsRequest,
) -> Result<GithubMetrics> {
    if request.metadata.repository != request.activity.repository {
        bail!("metadata и activity относятся к разным GitHub-репозиториям");
    }
    let activity = &request.activity;
    let mut warnings = activity.warnings.clone();
    let issue_closure_rate = ratio_metric(
        activity.issues_closed,
        activity.issues_opened,
        "percent",
        "issue closure rate",
        &mut warnings,
    );
    let pull_request_merge_rate = ratio_metric(
        activity.pull_requests_merged,
        activity.pull_requests_opened,
        "percent",
        "pull request merge rate",
        &mut warnings,
    );
    let median_merge_hours = if activity.merge_hours.is_empty() {
        unavailable("hours", "median merge time", &mut warnings)
    } else {
        let mut values = activity.merge_hours.clone();
        values.sort_by(f64::total_cmp);
        let middle = values.len() / 2;
        let value = if values.len().is_multiple_of(2) {
            (values[middle - 1] + values[middle]) / 2.0
        } else {
            values[middle]
        };
        derived(value, "hours")
    };
    let ci_success_rate = ratio_metric(
        activity.workflow_successes,
        activity.workflow_runs,
        "percent",
        "CI success rate",
        &mut warnings,
    );
    let release_cadence_days = match &activity.release_times {
        Some(times) if times.len() >= 2 => {
            let mut parsed = times
                .iter()
                .filter_map(|value| DateTime::parse_from_rfc3339(value).ok())
                .collect::<Vec<_>>();
            parsed.sort();
            let intervals = parsed
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).num_seconds().unsigned_abs() as f64 / 86_400.0)
                .collect::<Vec<_>>();
            derived(
                intervals.iter().sum::<f64>() / intervals.len() as f64,
                "days",
            )
        }
        _ => unavailable("days", "release cadence", &mut warnings),
    };

    Ok(GithubMetrics {
        repository: request.metadata.repository.clone(),
        period_days: activity.period_days,
        commits: source_metric(activity.commits, "commits", "commit count", &mut warnings),
        active_contributors: source_metric(
            activity.active_contributors,
            "contributors",
            "active contributors",
            &mut warnings,
        ),
        issue_closure_rate,
        pull_request_merge_rate,
        median_merge_hours,
        ci_success_rate,
        release_cadence_days,
        warnings,
    })
}

fn source_metric(
    value: Option<u64>,
    unit: &str,
    label: &str,
    warnings: &mut Vec<String>,
) -> MetricValue {
    match value {
        Some(value) => MetricValue {
            value: Some(value as f64),
            unit: unit.into(),
            kind: MetricKind::Source,
        },
        None => unavailable(unit, label, warnings),
    }
}

fn ratio_metric(
    numerator: Option<u64>,
    denominator: Option<u64>,
    unit: &str,
    label: &str,
    warnings: &mut Vec<String>,
) -> MetricValue {
    match (numerator, denominator) {
        (Some(_), Some(0)) | (_, None) | (None, _) => unavailable(unit, label, warnings),
        (Some(numerator), Some(denominator)) => {
            derived(numerator as f64 / denominator as f64 * 100.0, unit)
        }
    }
}

fn derived(value: f64, unit: &str) -> MetricValue {
    MetricValue {
        value: Some(value),
        unit: unit.into(),
        kind: MetricKind::Derived,
    }
}

fn unavailable(unit: &str, label: &str, warnings: &mut Vec<String>) -> MetricValue {
    warnings.push(format!("метрика {label} недоступна"));
    MetricValue {
        value: None,
        unit: unit.into(),
        kind: MetricKind::Unavailable,
    }
}

fn validate_repository(repository: &str) -> Result<()> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty()
        || name.is_empty()
        || parts.next().is_some()
        || !owner.chars().all(valid_repository_char)
        || !name.chars().all(valid_repository_char)
    {
        bail!("репозиторий должен иметь безопасный формат owner/name");
    }
    Ok(())
}

fn valid_repository_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

fn required_u64(value: &Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("GitHub не вернул поле {field}"))
}

fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("GitHub не вернул поле {field}"))
}

fn timestamp_at(value: &Value, field: &str) -> Option<DateTime<Utc>> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn commit_author_identity(value: &Value) -> Option<String> {
    value
        .pointer("/author/login")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .pointer("/commit/author/email")
                .and_then(Value::as_str)
        })
        .or_else(|| value.pointer("/commit/author/name").and_then(Value::as_str))
        .filter(|identity| !identity.is_empty())
        .map(|identity| identity.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::Query, http::StatusCode as AxumStatus, response::IntoResponse, routing::get, Json,
        Router,
    };
    use serde_json::json;
    use std::{collections::HashMap, sync::Arc};
    use tokio::{net::TcpListener, task::JoinHandle};

    async fn spawn(router: Router) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{address}"), task)
    }

    #[tokio::test]
    async fn metadata_is_normalized_and_http_errors_are_safe() {
        let router = Router::new()
            .route(
                "/repos/acme/demo",
                get(|| async {
                    Json(json!({
                        "description": "demo",
                        "stargazers_count": 12,
                        "forks_count": 3,
                        "subscribers_count": 2,
                        "open_issues_count": 4,
                        "default_branch": "main",
                        "created_at": "2025-01-01T00:00:00Z",
                        "pushed_at": "2026-09-20T00:00:00Z"
                    }))
                }),
            )
            .route(
                "/repos/acme/demo/languages",
                get(|| async { Json(json!({"Rust": 1000, "Shell": 20})) }),
            )
            .route(
                "/repos/acme/missing",
                get(|| async { (AxumStatus::NOT_FOUND, "secret response body") }),
            )
            .route(
                "/repos/acme/limited",
                get(|| async { (AxumStatus::FORBIDDEN, "sensitive rate-limit details") }),
            );
        let (base_url, task) = spawn(router).await;
        let client = GithubClient::with_base_url(&base_url).unwrap();
        let metadata = client
            .repository_metadata(&GithubRepositoryRequest {
                repository: "acme/demo".into(),
            })
            .await
            .unwrap();
        assert_eq!(metadata.stars, 12);
        assert_eq!(metadata.languages["Rust"], 1000);

        let missing = client
            .repository_metadata(&GithubRepositoryRequest {
                repository: "acme/missing".into(),
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(missing.contains("не найден"));
        assert!(!missing.contains("secret"));
        let limited = client
            .repository_metadata(&GithubRepositoryRequest {
                repository: "acme/limited".into(),
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(limited.contains("rate limit"));
        assert!(!limited.contains("sensitive"));
        task.abort();
    }

    #[tokio::test]
    async fn authentication_header_is_sent_and_never_exposed_in_errors() {
        const TOKEN: &str = "github-test-secret";
        async fn assert_auth(headers: axum::http::HeaderMap) {
            assert_eq!(
                headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer github-test-secret")
            );
        }
        let router = Router::new()
            .route(
                "/repos/acme/demo",
                get(|headers: axum::http::HeaderMap| async move {
                    assert_auth(headers).await;
                    Json(json!({
                        "description": "demo",
                        "stargazers_count": 1,
                        "forks_count": 2,
                        "subscribers_count": 3,
                        "open_issues_count": 4,
                        "default_branch": "main",
                        "created_at": "2025-01-01T00:00:00Z",
                        "pushed_at": null
                    }))
                }),
            )
            .route(
                "/repos/acme/demo/languages",
                get(|headers: axum::http::HeaderMap| async move {
                    assert_auth(headers).await;
                    Json(json!({"Rust": 10}))
                }),
            )
            .route(
                "/repos/acme/limited",
                get(|headers: axum::http::HeaderMap| async move {
                    assert_auth(headers).await;
                    (AxumStatus::FORBIDDEN, "github-test-secret")
                }),
            );
        let (base_url, task) = spawn(router).await;
        let client = GithubClient::with_base_url_and_token(&base_url, Some(TOKEN)).unwrap();
        let metadata = client
            .repository_metadata(&GithubRepositoryRequest {
                repository: "acme/demo".into(),
            })
            .await
            .unwrap();
        assert_eq!(metadata.languages["Rust"], 10);

        let error = client
            .repository_metadata(&GithubRepositoryRequest {
                repository: "acme/limited".into(),
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains(TOKEN));
        task.abort();
    }

    async fn paged_issues(Query(query): Query<HashMap<String, String>>) -> Json<Value> {
        if query.get("page").map(String::as_str) == Some("1") {
            let now = Utc::now().to_rfc3339();
            Json(Value::Array(
                (0..PAGE_SIZE)
                    .map(|_| json!({"created_at": now, "closed_at": now}))
                    .collect(),
            ))
        } else {
            Json(json!([]))
        }
    }

    #[tokio::test]
    async fn activity_handles_pagination_and_collects_all_sources() {
        let week = Utc::now().timestamp();
        let now = Utc::now();
        let created = (now - Duration::hours(12)).to_rfc3339();
        let merged = now.to_rfc3339();
        let release_old = (now - Duration::days(10)).to_rfc3339();
        let release_new = (now - Duration::days(2)).to_rfc3339();
        let router = Router::new()
            .route(
                "/repos/acme/demo/stats/commit_activity",
                get(move || async move { Json(json!([{"week": week, "total": 5}])) }),
            )
            .route(
                "/repos/acme/demo/stats/contributors",
                get(move || async move { Json(json!([{"weeks": [{"w": week, "c": 2}]}])) }),
            )
            .route("/repos/acme/demo/issues", get(paged_issues))
            .route(
                "/repos/acme/demo/pulls",
                get(move || {
                    let created = created.clone();
                    let merged = merged.clone();
                    async move {
                        Json(json!([{
                            "created_at": created,
                            "merged_at": merged,
                            "updated_at": merged
                        }]))
                    }
                }),
            )
            .route(
                "/repos/acme/demo/actions/runs",
                get(|| async { Json(json!({"workflow_runs": [{"conclusion": "success"}]})) }),
            )
            .route(
                "/repos/acme/demo/releases",
                get(move || {
                    let old = release_old.clone();
                    let new = release_new.clone();
                    async move {
                        Json(json!([
                            {"published_at": old},
                            {"published_at": new}
                        ]))
                    }
                }),
            );
        let (base_url, task) = spawn(router).await;
        let client = GithubClient::with_base_url(&base_url).unwrap();
        let activity = client
            .project_activity(&GithubActivityRequest {
                repository: "acme/demo".into(),
                period_days: 30,
            })
            .await
            .unwrap();
        assert_eq!(activity.commits, Some(5));
        assert_eq!(activity.active_contributors, Some(1));
        assert_eq!(activity.issues_opened, Some(PAGE_SIZE as u64));
        assert_eq!(activity.issues_closed, Some(PAGE_SIZE as u64));
        assert_eq!(activity.pull_requests_merged, Some(1));
        assert_eq!(activity.workflow_successes, Some(1));
        assert_eq!(activity.release_times.as_ref().unwrap().len(), 2);
        assert!(!activity.truncated);
        task.abort();
    }

    #[tokio::test]
    async fn activity_validates_period_and_degrades_persistent_pending_statistics() {
        let pending_requests = Arc::new(AtomicUsize::new(0));
        let pending_requests_for_route = pending_requests.clone();
        let router = Router::new()
            .route(
                "/repos/acme/demo/stats/commit_activity",
                get(move || {
                    pending_requests_for_route.fetch_add(1, Ordering::SeqCst);
                    async { AxumStatus::ACCEPTED }
                }),
            )
            .route(
                "/repos/acme/demo/stats/contributors",
                get(|| async { AxumStatus::ACCEPTED }),
            )
            .route(
                "/repos/acme/demo/commits",
                get(|| async {
                    Json(json!([
                        {"author": {"login": "Alice"}},
                        {"author": {"login": "alice"}},
                        {"author": null, "commit": {"author": {"email": "bob@example.test"}}}
                    ]))
                }),
            );
        let (base_url, task) = spawn(router).await;
        let client = GithubClient::with_base_url(&base_url)
            .unwrap()
            .with_stats_retry_delays(vec![StdDuration::ZERO; 3]);
        for period_days in [0, 366] {
            let error = client
                .project_activity(&GithubActivityRequest {
                    repository: "acme/demo".into(),
                    period_days,
                })
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("от 1 до 365"));
        }
        let activity = client
            .project_activity(&GithubActivityRequest {
                repository: "acme/demo".into(),
                period_days: 30,
            })
            .await
            .unwrap();
        assert_eq!(pending_requests.load(Ordering::SeqCst), 4);
        assert_eq!(activity.commits, None);
        assert_eq!(activity.active_contributors, Some(2));
        assert!(activity.incomplete_sources.contains(&"commits".into()));
        assert!(!activity.incomplete_sources.contains(&"contributors".into()));
        assert!(activity
            .warnings
            .iter()
            .any(|warning| warning.contains("commits недоступны")));
        assert!(activity
            .warnings
            .iter()
            .any(|warning| warning.contains("уникальным авторам коммитов")));

        let metrics = calculate_github_metrics(&CalculateGithubMetricsRequest {
            metadata: metadata(),
            activity,
        })
        .unwrap();
        assert_eq!(metrics.commits.kind, MetricKind::Unavailable);
        assert_eq!(metrics.active_contributors.value, Some(2.0));
        task.abort();
    }

    #[tokio::test]
    async fn pagination_stops_when_a_page_reaches_the_time_boundary() {
        let requests = Arc::new(AtomicUsize::new(0));
        let route_requests = requests.clone();
        let old = (Utc::now() - Duration::days(60)).to_rfc3339();
        let router = Router::new().route(
            "/items",
            get(move || {
                route_requests.fetch_add(1, Ordering::SeqCst);
                let old = old.clone();
                async move {
                    Json(Value::Array(
                        (0..PAGE_SIZE).map(|_| json!({"updated_at": old})).collect(),
                    ))
                }
            }),
        );
        let (base_url, task) = spawn(router).await;
        let client = GithubClient::with_base_url(&base_url).unwrap();
        let budget = AtomicUsize::new(0);
        let result = client
            .fetch_pages(
                "/items?per_page=100",
                None,
                Some(("updated_at", Utc::now() - Duration::days(30))),
                &budget,
            )
            .await
            .unwrap();

        assert_eq!(result.values.len(), PAGE_SIZE);
        assert!(!result.truncated);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        task.abort();
    }

    #[tokio::test]
    async fn request_budget_is_bounded() {
        let router = Router::new()
            .route(
                "/repos/acme/demo",
                get(|| async { Json(json!({"stargazers_count": 1})) }),
            )
            .route(
                "/repos/acme/demo/languages",
                get(|| async { Json(json!({})) }),
            );
        let (base_url, task) = spawn(router).await;
        let client = GithubClient::with_base_url(&base_url)
            .unwrap()
            .with_request_limit(1);
        let error = client
            .repository_metadata(&GithubRepositoryRequest {
                repository: "acme/demo".into(),
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("внутренний лимит"));
        task.abort();
    }

    fn metadata() -> GithubRepositoryMetadata {
        GithubRepositoryMetadata {
            repository: "acme/demo".into(),
            description: None,
            stars: 10,
            forks: 2,
            subscribers: 1,
            open_issues_and_pulls: 3,
            default_branch: "main".into(),
            created_at: "2025-01-01T00:00:00Z".into(),
            pushed_at: None,
            languages: BTreeMap::from([("Rust".into(), 100)]),
        }
    }

    fn activity() -> GithubProjectActivity {
        GithubProjectActivity {
            repository: "acme/demo".into(),
            period_days: 30,
            since: "2026-08-25T00:00:00Z".into(),
            commits: Some(20),
            active_contributors: Some(4),
            issues_opened: Some(10),
            issues_closed: Some(8),
            pull_requests_opened: Some(5),
            pull_requests_merged: Some(4),
            merge_hours: vec![10.0, 20.0, 30.0],
            workflow_runs: Some(10),
            workflow_successes: Some(9),
            release_times: Some(vec![
                "2026-09-01T00:00:00Z".into(),
                "2026-09-11T00:00:00Z".into(),
            ]),
            truncated: false,
            incomplete_sources: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn metrics_are_deterministic_and_missing_values_are_not_zero() {
        let metrics = calculate_github_metrics(&CalculateGithubMetricsRequest {
            metadata: metadata(),
            activity: activity(),
        })
        .unwrap();
        assert_eq!(metrics.commits.value, Some(20.0));
        assert_eq!(metrics.issue_closure_rate.value, Some(80.0));
        assert_eq!(metrics.pull_request_merge_rate.value, Some(80.0));
        assert_eq!(metrics.median_merge_hours.value, Some(20.0));
        assert_eq!(metrics.ci_success_rate.value, Some(90.0));
        assert_eq!(metrics.release_cadence_days.value, Some(10.0));

        let mut partial = activity();
        partial.workflow_runs = None;
        partial.workflow_successes = None;
        partial.release_times = None;
        partial.truncated = true;
        partial.warnings.push("данные усечены".into());
        let metrics = calculate_github_metrics(&CalculateGithubMetricsRequest {
            metadata: metadata(),
            activity: partial,
        })
        .unwrap();
        assert_eq!(metrics.ci_success_rate.value, None);
        assert_eq!(metrics.ci_success_rate.kind, MetricKind::Unavailable);
        assert!(metrics
            .warnings
            .iter()
            .any(|warning| warning.contains("усечены")));
    }
}
