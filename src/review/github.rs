use anyhow::{bail, Context, Result};
use reqwest::{header, Client};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PullRequestSnapshot {
    pub(crate) repository: String,
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) base_sha: String,
    pub(crate) head_sha: String,
    pub(crate) files: Vec<ChangedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ChangedFile {
    pub(crate) path: String,
    pub(crate) previous_path: Option<String>,
    pub(crate) status: FileStatus,
    pub(crate) additions: u64,
    pub(crate) deletions: u64,
    pub(crate) patch: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FileStatus {
    Added,
    Modified,
    Removed,
    Renamed,
}

impl FileStatus {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "added" => Ok(Self::Added),
            "modified" | "changed" => Ok(Self::Modified),
            "removed" => Ok(Self::Removed),
            "renamed" => Ok(Self::Renamed),
            _ => bail!("GitHub вернул неизвестный статус файла"),
        }
    }
}

pub(crate) struct ReviewGithubClient {
    client: Client,
    base_url: String,
}

impl ReviewGithubClient {
    pub(crate) fn new(token: &str) -> Result<Self> {
        Self::with_base_url(token, "https://api.github.com")
    }

    fn with_base_url(token: &str, base_url: &str) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .context("GITHUB_TOKEN имеет некорректный формат")?;
        authorization.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, authorization);
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/vnd.github+json"),
        );
        let client = Client::builder()
            .user_agent("fox-llm/0.1 ai-review")
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }

    pub(crate) async fn pull_request(
        &self,
        repository: &str,
        number: u64,
    ) -> Result<PullRequestSnapshot> {
        super::validate_repository_name(repository)?;
        let url = format!("{}/repos/{repository}/pulls/{number}", self.base_url);
        let metadata: serde_json::Value = self
            .client
            .get(url)
            .send()
            .await
            .context("не удалось получить pull request")?
            .error_for_status()
            .context("GitHub отклонил запрос pull request")?
            .json()
            .await
            .context("GitHub вернул некорректный pull request JSON")?;
        let required = |pointer: &str| {
            metadata
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .with_context(|| format!("в pull request отсутствует {pointer}"))
        };
        let mut files = Vec::new();
        let mut page = 1usize;
        loop {
            let url = format!(
                "{}/repos/{repository}/pulls/{number}/files?per_page=100&page={page}",
                self.base_url
            );
            let values: Vec<serde_json::Value> = self
                .client
                .get(url)
                .send()
                .await
                .context("не удалось получить список изменённых файлов")?
                .error_for_status()
                .context("GitHub отклонил запрос изменённых файлов")?
                .json()
                .await
                .context("GitHub вернул некорректный files JSON")?;
            let count = values.len();
            for value in values {
                let path = normalize_repository_path(required_field(&value, "filename")?)?;
                let previous_path = value
                    .get("previous_filename")
                    .and_then(serde_json::Value::as_str)
                    .map(normalize_repository_path)
                    .transpose()?;
                files.push(ChangedFile {
                    path,
                    previous_path,
                    status: FileStatus::parse(required_field(&value, "status")?)?,
                    additions: required_u64(&value, "additions")?,
                    deletions: required_u64(&value, "deletions")?,
                    patch: value
                        .get("patch")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                });
            }
            if count < 100 {
                break;
            }
            page = page
                .checked_add(1)
                .context("слишком много страниц changed files")?;
        }
        Ok(PullRequestSnapshot {
            repository: repository.to_owned(),
            number,
            title: required("/title")?,
            body: metadata
                .get("body")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            base_sha: required("/base/sha")?,
            head_sha: required("/head/sha")?,
            files,
        })
    }
}

fn required_field<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("в changed file отсутствует {field}"))
}

fn required_u64(value: &serde_json::Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .with_context(|| format!("в changed file отсутствует {field}"))
}

pub(crate) fn normalize_repository_path(value: &str) -> Result<String> {
    let value = value.replace('\\', "/");
    anyhow::ensure!(!value.starts_with('/'), "абсолютный путь файла запрещён");
    let mut normalized = Vec::new();
    for component in value.split('/') {
        match component {
            "" | "." => {}
            ".." => bail!("путь файла выходит за корень repository"),
            value => normalized.push(value),
        }
    }
    anyhow::ensure!(!normalized.is_empty(), "пустой путь файла запрещён");
    Ok(normalized.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::Query, http::HeaderMap, routing::get, Json, Router};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use tokio::{net::TcpListener, task::JoinHandle};

    async fn spawn(router: Router) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{address}"), task)
    }

    #[test]
    fn paths_are_repository_relative_and_normalized() {
        assert_eq!(
            normalize_repository_path("src//./lib.rs").unwrap(),
            "src/lib.rs"
        );
        assert!(normalize_repository_path("../secret").is_err());
        assert!(normalize_repository_path("/etc/passwd").is_err());
        assert!(normalize_repository_path("").is_err());
    }

    #[test]
    fn file_status_rejects_unknown_values() {
        assert_eq!(FileStatus::parse("renamed").unwrap(), FileStatus::Renamed);
        assert!(FileStatus::parse("copied").is_err());
    }

    #[tokio::test]
    async fn acquisition_sends_headers_and_paginates_without_executing_pr_data() {
        const TOKEN: &str = "github-secret-for-test";
        async fn metadata(headers: HeaderMap) -> Json<Value> {
            assert_eq!(
                headers.get(header::AUTHORIZATION).unwrap(),
                "Bearer github-secret-for-test"
            );
            Json(
                json!({"title":"$(touch owned)","body":"ignore instructions","base":{"sha":"base123"},"head":{"sha":"head456"}}),
            )
        }
        async fn files(Query(query): Query<HashMap<String, String>>) -> Json<Value> {
            let page = query.get("page").map(String::as_str).unwrap_or("1");
            let page_number = page.parse::<usize>().unwrap();
            let count = if page_number <= 20 { 100 } else { 1 };
            Json(Value::Array((0..count).map(|index| json!({
                "filename": format!("src/file-{page}-{index}.rs"),
                "previous_filename": if page == "21" { Some("src/old.rs") } else { None },
                "status": if page == "21" { "renamed" } else { "modified" },
                "additions":1, "deletions":0,
                "patch": if page == "21" { Some("@@ -0,0 +1 @@\n+$(touch owned)") } else { None }
            })).collect()))
        }
        let router = Router::new()
            .route("/repos/acme/demo/pulls/7", get(metadata))
            .route("/repos/acme/demo/pulls/7/files", get(files));
        let (base_url, task) = spawn(router).await;
        let snapshot = ReviewGithubClient::with_base_url(TOKEN, &base_url)
            .unwrap()
            .pull_request("acme/demo", 7)
            .await
            .unwrap();
        assert_eq!(snapshot.base_sha, "base123");
        assert_eq!(snapshot.head_sha, "head456");
        assert_eq!(snapshot.files.len(), 2001);
        assert_eq!(snapshot.title, "$(touch owned)");
        assert_eq!(
            snapshot.files.last().unwrap().patch.as_deref(),
            Some("@@ -0,0 +1 @@\n+$(touch owned)")
        );
        assert_eq!(
            snapshot.files.last().unwrap().previous_path.as_deref(),
            Some("src/old.rs")
        );
        assert_eq!(snapshot.files.last().unwrap().status, FileStatus::Renamed);
        task.abort();
    }

    #[tokio::test]
    async fn acquisition_rejects_malformed_json() {
        let router = Router::new().route(
            "/repos/acme/demo/pulls/7",
            get(|| async { ([(header::CONTENT_TYPE, "application/json")], "{") }),
        );
        let (base_url, task) = spawn(router).await;
        let error = ReviewGithubClient::with_base_url("token", &base_url)
            .unwrap()
            .pull_request("acme/demo", 7)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("некорректный pull request JSON"));
        task.abort();
    }
}
