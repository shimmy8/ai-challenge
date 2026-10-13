mod analysis;
mod cache;
mod diff;
mod github;
mod provider;
mod rag;
mod settings;
mod test_report;

pub(crate) use analysis::*;
pub(crate) use cache::*;
pub(crate) use diff::*;
pub(crate) use github::*;
pub(crate) use provider::*;
pub(crate) use rag::*;
pub(crate) use settings::*;
pub(crate) use test_report::*;

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewOptions {
    pub(crate) repository: String,
    pub(crate) pull_request: u64,
    pub(crate) test_report: Option<PathBuf>,
    pub(crate) output: PathBuf,
    pub(crate) json_output: PathBuf,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) embedding_model: Option<String>,
    pub(crate) embedding_cache: Option<PathBuf>,
    pub(crate) max_files: Option<usize>,
    pub(crate) max_diff_chars: Option<usize>,
    pub(crate) max_file_chars: Option<usize>,
    pub(crate) batch_chars: Option<usize>,
    pub(crate) max_findings: Option<usize>,
}

pub fn parse_review_options(args: &[String]) -> Result<ReviewOptions> {
    let mut repository = None;
    let mut pull_request = None;
    let mut test_report = None;
    let mut output = None;
    let mut json_output = None;
    let mut provider = None;
    let mut model = None;
    let mut embedding_model = None;
    let mut embedding_cache = None;
    let mut max_files = None;
    let mut max_diff_chars = None;
    let mut max_file_chars = None;
    let mut batch_chars = None;
    let mut max_findings = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .with_context(|| format!("для {flag} не указано значение"))?
            .clone();
        let slot = match flag {
            "--repository" => &mut repository,
            "--test-report" => &mut test_report,
            "--output" => &mut output,
            "--json-output" => &mut json_output,
            "--provider" => &mut provider,
            "--model" => &mut model,
            "--embedding-model" => &mut embedding_model,
            "--embedding-cache" => &mut embedding_cache,
            "--max-files" | "--max-diff-chars" | "--max-file-chars" | "--batch-chars"
            | "--max-findings" => {
                let parsed = value
                    .parse::<usize>()
                    .with_context(|| format!("{flag} должен быть положительным числом"))?;
                anyhow::ensure!(parsed > 0, "{flag} должен быть больше нуля");
                let target = match flag {
                    "--max-files" => &mut max_files,
                    "--max-diff-chars" => &mut max_diff_chars,
                    "--max-file-chars" => &mut max_file_chars,
                    "--batch-chars" => &mut batch_chars,
                    "--max-findings" => &mut max_findings,
                    _ => unreachable!(),
                };
                anyhow::ensure!(
                    target.replace(parsed).is_none(),
                    "аргумент указан повторно: {flag}"
                );
                index += 2;
                continue;
            }
            "--pull-request" => {
                anyhow::ensure!(pull_request.is_none(), "аргумент указан повторно: {flag}");
                let number = value
                    .parse::<u64>()
                    .with_context(|| "номер pull request должен быть положительным числом")?;
                anyhow::ensure!(number > 0, "номер pull request должен быть больше нуля");
                pull_request = Some(number);
                index += 2;
                continue;
            }
            _ => bail!("неизвестный аргумент review-pr: {flag}"),
        };
        anyhow::ensure!(slot.is_none(), "аргумент указан повторно: {flag}");
        anyhow::ensure!(
            !value.trim().is_empty(),
            "для {flag} указано пустое значение"
        );
        *slot = Some(value);
        index += 2;
    }
    let repository = repository.context("не указан --repository")?;
    validate_repository_name(&repository)?;
    Ok(ReviewOptions {
        repository,
        pull_request: pull_request.context("не указан --pull-request")?,
        test_report: test_report.map(PathBuf::from),
        output: PathBuf::from(output.context("не указан --output")?),
        json_output: PathBuf::from(json_output.context("не указан --json-output")?),
        provider,
        model,
        embedding_model,
        embedding_cache: embedding_cache.map(PathBuf::from),
        max_files,
        max_diff_chars,
        max_file_chars,
        batch_chars,
        max_findings,
    })
}

fn validate_repository_name(value: &str) -> Result<()> {
    let Some((owner, repository)) = value.split_once('/') else {
        bail!("repository должен иметь формат owner/name");
    };
    anyhow::ensure!(
        !owner.is_empty() && !repository.is_empty() && !repository.contains('/'),
        "repository должен иметь формат owner/name"
    );
    Ok(())
}

pub async fn run(options: ReviewOptions) -> Result<()> {
    run_review(options).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_review_options_with_optional_test_report() {
        let options = parse_review_options(&args(&[
            "--repository",
            "owner/repo",
            "--pull-request",
            "42",
            "--test-report",
            "test-report.json",
            "--output",
            "review.md",
            "--json-output",
            "review.json",
            "--embedding-cache",
            ".ai-review-cache/review-rag.db",
        ]))
        .unwrap();

        assert_eq!(options.repository, "owner/repo");
        assert_eq!(options.pull_request, 42);
        assert_eq!(options.test_report, Some(PathBuf::from("test-report.json")));
        assert_eq!(options.output, PathBuf::from("review.md"));
        assert_eq!(options.json_output, PathBuf::from("review.json"));
        assert_eq!(options.provider, None);
        assert_eq!(
            options.embedding_cache,
            Some(PathBuf::from(".ai-review-cache/review-rag.db"))
        );
    }

    #[test]
    fn review_options_require_repository_pr_and_outputs() {
        for missing in [
            "--repository",
            "--pull-request",
            "--output",
            "--json-output",
        ] {
            let mut values = args(&[
                "--repository",
                "owner/repo",
                "--pull-request",
                "42",
                "--output",
                "review.md",
                "--json-output",
                "review.json",
            ]);
            let index = values.iter().position(|value| value == missing).unwrap();
            values.drain(index..=index + 1);
            assert!(parse_review_options(&values).is_err(), "missing {missing}");
        }
    }

    #[test]
    fn review_options_parse_limit_overrides() {
        let options = parse_review_options(&args(&[
            "--repository",
            "owner/repo",
            "--pull-request",
            "42",
            "--output",
            "review.md",
            "--json-output",
            "review.json",
            "--max-files",
            "7",
            "--batch-chars",
            "1234",
        ]))
        .unwrap();
        assert_eq!(options.max_files, Some(7));
        assert_eq!(options.batch_chars, Some(1234));
        assert!(parse_review_options(&args(&[
            "--repository",
            "owner/repo",
            "--pull-request",
            "42",
            "--output",
            "review.md",
            "--json-output",
            "review.json",
            "--max-files",
            "0",
        ]))
        .is_err());
    }
}
