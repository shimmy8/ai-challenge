use anyhow::{bail, Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

use super::{
    GithubMetrics, GithubProjectActivity, GithubRepositoryMetadata, MetricKind, MetricValue,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct RenderGithubReportRequest {
    pub(crate) metadata: GithubRepositoryMetadata,
    pub(crate) activity: GithubProjectActivity,
    pub(crate) metrics: GithubMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct SaveReportRequest {
    /// A single filename in the MCP server working directory, not a path.
    pub(crate) filename: String,
    /// Arbitrary UTF-8 report content.
    pub(crate) content: String,
    /// Existing regular files are protected unless this is explicitly true.
    #[serde(default)]
    pub(crate) overwrite: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct SaveReportResult {
    pub(crate) path: String,
    pub(crate) bytes_written: usize,
}

pub(crate) fn render_github_report(request: &RenderGithubReportRequest) -> Result<String> {
    if request.metadata.repository != request.metrics.repository
        || request.metadata.repository != request.activity.repository
    {
        bail!("metadata, activity и metrics относятся к разным GitHub-репозиториям");
    }
    if request.activity.period_days != request.metrics.period_days {
        bail!("activity и metrics относятся к разным периодам");
    }
    let languages = if request.metadata.languages.is_empty() {
        "unavailable".to_owned()
    } else {
        let total = request.metadata.languages.values().sum::<u64>();
        request
            .metadata
            .languages
            .iter()
            .map(|(language, bytes)| {
                let share = if total == 0 {
                    0.0
                } else {
                    *bytes as f64 / total as f64 * 100.0
                };
                format!("{language}: {bytes} bytes ({share:.1}%)")
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let description = request
        .metadata
        .description
        .as_deref()
        .unwrap_or("unavailable");
    let pushed_at = request
        .metadata
        .pushed_at
        .as_deref()
        .unwrap_or("unavailable");
    let releases = request
        .activity
        .release_times
        .as_ref()
        .map(|values| values.len() as u64);
    let mut report = format!(
        "# GitHub project report: {}\n\n\
         Period: {} days\n\n\
         ## Overview\n\n\
         - Repository: https://github.com/{}\n\
         - Description: {}\n\
         - Created at: {}\n\
         - Last pushed at: {}\n\
         - Stars: {} (source)\n\
         - Forks: {} (source)\n\
         - Subscribers: {} (source)\n\
         - Open issues and pull requests: {} (source)\n\
         - Default branch: {} (source)\n\
         - Languages: {} (source)\n\n\
         ## Development\n\n\
         - Commits: {}\n\
         - Active contributors: {}\n\n\
         ## Collaboration\n\n\
         - Issues opened: {}\n\
         - Issues closed: {}\n\
         - Issue closure rate: {}\n\
         - Pull requests opened: {}\n\
         - Pull requests merged: {}\n\
         - Pull request merge rate: {}\n\
         - Median merge time: {}\n\n\
         ## Delivery\n\n\
         - Workflow runs: {}\n\
         - Successful workflow runs: {}\n\
         - CI success rate: {}\n\
         - Releases: {}\n\
         - Release cadence: {}\n\n\
         ## Data coverage\n\n{}\n\
         ## Warnings\n\n",
        request.metadata.repository,
        request.metrics.period_days,
        request.metadata.repository,
        description,
        request.metadata.created_at,
        pushed_at,
        request.metadata.stars,
        request.metadata.forks,
        request.metadata.subscribers,
        request.metadata.open_issues_and_pulls,
        request.metadata.default_branch,
        languages,
        format_metric(&request.metrics.commits),
        format_metric(&request.metrics.active_contributors),
        format_count(request.activity.issues_opened, "issues"),
        format_count(request.activity.issues_closed, "issues"),
        format_metric(&request.metrics.issue_closure_rate),
        format_count(request.activity.pull_requests_opened, "pull requests"),
        format_count(request.activity.pull_requests_merged, "pull requests"),
        format_metric(&request.metrics.pull_request_merge_rate),
        format_metric(&request.metrics.median_merge_hours),
        format_count(request.activity.workflow_runs, "runs"),
        format_count(request.activity.workflow_successes, "runs"),
        format_metric(&request.metrics.ci_success_rate),
        format_count(releases, "releases"),
        format_metric(&request.metrics.release_cadence_days),
        format_coverage(&request.activity),
    );
    if request.metrics.warnings.is_empty() {
        report.push_str("- None\n");
    } else {
        for warning in &request.metrics.warnings {
            report.push_str("- ");
            report.push_str(warning);
            report.push('\n');
        }
    }
    Ok(report)
}

fn format_count(value: Option<u64>, unit: &str) -> String {
    value
        .map(|value| format!("{value} {unit} (source)"))
        .unwrap_or_else(|| "unavailable (unavailable)".into())
}

fn format_coverage(activity: &GithubProjectActivity) -> String {
    [
        "commits",
        "contributors",
        "issues",
        "pull_requests",
        "actions",
        "releases",
    ]
    .into_iter()
    .map(|source| {
        let status = if activity
            .incomplete_sources
            .iter()
            .any(|value| value == source)
        {
            "incomplete"
        } else {
            "complete"
        };
        format!("- {source}: {status}")
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn format_metric(metric: &MetricValue) -> String {
    let kind = match metric.kind {
        MetricKind::Source => "source",
        MetricKind::Derived => "derived",
        MetricKind::Unavailable => "unavailable",
    };
    match metric.value {
        Some(value) => format!("{value:.2} {} ({kind})", metric.unit),
        None => format!("unavailable ({kind})"),
    }
}

pub(crate) fn save_report_in_directory(
    directory: &Path,
    request: &SaveReportRequest,
) -> Result<SaveReportResult> {
    validate_filename(&request.filename)?;
    let directory = directory
        .canonicalize()
        .context("рабочий каталог отчёта недоступен")?;
    let target = directory.join(&request.filename);
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            bail!("нельзя сохранять отчёт через символическую ссылку");
        }
        if !metadata.is_file() {
            bail!("цель отчёта не является обычным файлом");
        }
        if !request.overwrite {
            bail!("файл уже существует; для замены укажите overwrite=true");
        }
    }
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if request.overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = options
        .open(&target)
        .context("не удалось открыть файл отчёта для записи")?;
    use std::io::Write;
    file.write_all(request.content.as_bytes())
        .context("не удалось записать отчёт")?;
    file.sync_all()
        .context("не удалось синхронизировать файл отчёта")?;
    Ok(SaveReportResult {
        path: target.display().to_string(),
        bytes_written: request.content.len(),
    })
}

fn validate_filename(filename: &str) -> Result<()> {
    if filename.is_empty()
        || matches!(filename, "." | "..")
        || filename.contains('/')
        || filename.contains('\\')
        || Path::new(filename).is_absolute()
    {
        bail!("filename должен быть безопасным именем одного файла, а не путём");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GithubMetrics, GithubProjectActivity, MetricKind, MetricValue};
    use std::collections::BTreeMap;

    fn metric(value: Option<f64>, unit: &str, kind: MetricKind) -> MetricValue {
        MetricValue {
            value,
            unit: unit.into(),
            kind,
        }
    }

    fn report_request() -> RenderGithubReportRequest {
        RenderGithubReportRequest {
            metadata: GithubRepositoryMetadata {
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
            },
            activity: GithubProjectActivity {
                repository: "acme/demo".into(),
                period_days: 30,
                since: "2026-08-25T00:00:00Z".into(),
                commits: Some(20),
                active_contributors: Some(4),
                issues_opened: Some(10),
                issues_closed: Some(8),
                pull_requests_opened: Some(5),
                pull_requests_merged: Some(4),
                merge_hours: vec![12.0],
                workflow_runs: Some(10),
                workflow_successes: Some(9),
                release_times: Some(vec!["2026-09-01T00:00:00Z".into()]),
                truncated: false,
                incomplete_sources: vec!["releases".into()],
                warnings: vec!["release cadence недоступна".into()],
            },
            metrics: GithubMetrics {
                repository: "acme/demo".into(),
                period_days: 30,
                commits: metric(Some(20.0), "commits", MetricKind::Source),
                active_contributors: metric(Some(4.0), "contributors", MetricKind::Source),
                issue_closure_rate: metric(Some(80.0), "percent", MetricKind::Derived),
                pull_request_merge_rate: metric(None, "percent", MetricKind::Unavailable),
                median_merge_hours: metric(Some(12.0), "hours", MetricKind::Derived),
                ci_success_rate: metric(Some(90.0), "percent", MetricKind::Derived),
                release_cadence_days: metric(None, "days", MetricKind::Unavailable),
                warnings: vec!["release cadence недоступна".into()],
            },
        }
    }

    #[test]
    fn github_report_has_all_sections_and_metric_kinds() {
        let report = render_github_report(&report_request()).unwrap();
        let expected = [
            "# GitHub project report: acme/demo",
            "## Overview",
            "## Development",
            "## Collaboration",
            "## Delivery",
            "## Data coverage",
            "## Warnings",
            "20.00 commits (source)",
            "Issues opened: 10 issues (source)",
            "Workflow runs: 10 runs (source)",
            "Rust: 100 bytes (100.0%)",
            "releases: incomplete",
            "80.00 percent (derived)",
            "unavailable (unavailable)",
            "release cadence недоступна",
        ];
        for fragment in expected {
            assert!(report.contains(fragment), "missing fragment: {fragment}");
        }
    }

    #[test]
    fn save_report_accepts_arbitrary_utf8_and_protects_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let request = SaveReportRequest {
            filename: "metrics.json".into(),
            content: "{\"статус\":\"готово\"}".into(),
            overwrite: false,
        };
        let result = save_report_in_directory(directory.path(), &request).unwrap();
        assert_eq!(result.bytes_written, request.content.len());
        assert_eq!(fs::read_to_string(&result.path).unwrap(), request.content);

        let error = save_report_in_directory(directory.path(), &request)
            .unwrap_err()
            .to_string();
        assert!(error.contains("overwrite=true"));
        let replacement = SaveReportRequest {
            content: "замена".into(),
            overwrite: true,
            ..request
        };
        save_report_in_directory(directory.path(), &replacement).unwrap();
        assert_eq!(fs::read_to_string(result.path).unwrap(), "замена");
    }

    #[test]
    fn save_report_rejects_paths_and_non_file_targets() {
        let directory = tempfile::tempdir().unwrap();
        for filename in [
            "",
            ".",
            "..",
            "/tmp/report",
            "nested/report",
            "nested\\report",
        ] {
            let error = save_report_in_directory(
                directory.path(),
                &SaveReportRequest {
                    filename: filename.into(),
                    content: "x".into(),
                    overwrite: false,
                },
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("безопасным именем"));
        }
        fs::create_dir(directory.path().join("directory-target")).unwrap();
        let error = save_report_in_directory(
            directory.path(),
            &SaveReportRequest {
                filename: "directory-target".into(),
                content: "x".into(),
                overwrite: true,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("обычным файлом"));
    }

    #[cfg(unix)]
    #[test]
    fn save_report_rejects_symlink_target() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), directory.path().join("report.txt")).unwrap();
        let error = save_report_in_directory(
            directory.path(),
            &SaveReportRequest {
                filename: "report.txt".into(),
                content: "x".into(),
                overwrite: true,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("символическую ссылку"));
    }
}
