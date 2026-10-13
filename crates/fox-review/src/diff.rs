use super::{ChangedFile, ReviewSettings};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FilePriority {
    Production,
    Configuration,
    Test,
    Documentation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SelectedFile {
    pub(crate) path: String,
    pub(crate) patch: String,
    pub(crate) priority: FilePriority,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct Coverage {
    pub(crate) total_files: usize,
    pub(crate) selected_files: usize,
    pub(crate) skipped_files: usize,
    pub(crate) selected_chars: usize,
    pub(crate) partial: bool,
    pub(crate) notes: Vec<String>,
}

pub(crate) fn select_files(
    files: &[ChangedFile],
    settings: &ReviewSettings,
) -> (Vec<SelectedFile>, Coverage) {
    let mut candidates = Vec::new();
    let mut notes = Vec::new();
    for file in files {
        let Some(priority) = classify_path(&file.path) else {
            notes.push(format!(
                "{}: исключён binary/vendor/generated/lock",
                file.path
            ));
            continue;
        };
        let Some(patch) = &file.patch else {
            notes.push(format!("{}: patch недоступен", file.path));
            continue;
        };
        let (patch, truncated) = truncate_patch(patch, settings.max_file_chars);
        if truncated {
            notes.push(format!("{}: patch усечён", file.path));
        }
        candidates.push(SelectedFile {
            path: file.path.clone(),
            patch,
            priority,
            truncated,
        });
    }
    candidates.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut selected = Vec::new();
    let mut chars = 0;
    for candidate in candidates {
        let count = candidate.patch.chars().count();
        if selected.len() >= settings.max_files || chars + count > settings.max_diff_chars {
            notes.push(format!("{}: превышен лимит review", candidate.path));
            continue;
        }
        chars += count;
        selected.push(candidate);
    }
    let selected_files = selected.len();
    let coverage = Coverage {
        total_files: files.len(),
        selected_files,
        skipped_files: files.len().saturating_sub(selected_files),
        selected_chars: chars,
        partial: selected_files < files.len() || selected.iter().any(|file| file.truncated),
        notes,
    };
    (selected, coverage)
}

pub(crate) fn classify_path(path: &str) -> Option<FilePriority> {
    let lower = path.to_ascii_lowercase();
    let filename = lower.rsplit('/').next().unwrap_or(&lower);
    if lower.split('/').any(|part| {
        matches!(
            part,
            "vendor" | "node_modules" | "target" | "dist" | "generated"
        )
    }) || matches!(
        filename,
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml"
    ) || [
        ".png", ".jpg", ".jpeg", ".gif", ".pdf", ".zip", ".woff", ".ico",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
        || lower.contains(".generated.")
        || lower.ends_with(".min.js")
    {
        return None;
    }
    if lower.ends_with(".md") || lower.starts_with("docs/") || lower.starts_with("reports/") {
        return Some(FilePriority::Documentation);
    }
    if lower.contains("test") || lower.starts_with("tests/") {
        return Some(FilePriority::Test);
    }
    if [".yml", ".yaml", ".toml", ".json", ".ini"]
        .iter()
        .any(|extension| lower.ends_with(extension))
    {
        return Some(FilePriority::Configuration);
    }
    Some(FilePriority::Production)
}

fn truncate_patch(patch: &str, limit: usize) -> (String, bool) {
    if patch.chars().count() <= limit {
        return (patch.replace("\r\n", "\n"), false);
    }
    let prefix: String = patch.chars().take(limit).collect();
    let boundary = prefix.rfind("\n@@ ").unwrap_or(prefix.len());
    let safe = if boundary == 0 {
        prefix
    } else {
        prefix[..boundary].to_owned()
    };
    (safe, true)
}

pub(crate) fn build_batches(files: &[SelectedFile], limit: usize) -> Vec<Vec<SelectedFile>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut chars = 0;
    for file in files {
        let file_chars = file.patch.chars().count();
        if !current.is_empty() && chars + file_chars > limit {
            batches.push(std::mem::take(&mut current));
            chars = 0;
        }
        chars += file_chars;
        current.push(file.clone());
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

pub(crate) fn retrieval_query(files: &[SelectedFile], diagnostics: &[String]) -> String {
    let mut parts = Vec::new();
    let mut identifiers = BTreeSet::new();
    for file in files {
        parts.push(file.path.clone());
        for line in file.patch.lines() {
            if line.starts_with("@@") {
                parts.push(line.to_owned());
            }
            if (line.starts_with('+') || line.starts_with('-'))
                && !line.starts_with("+++")
                && !line.starts_with("---")
            {
                identifiers.extend(
                    line[1..]
                        .split(|character: char| !character.is_alphanumeric() && character != '_')
                        .filter(|identifier| identifier.len() >= 3)
                        .take(30)
                        .map(str::to_owned),
                );
            }
        }
    }
    parts.extend(identifiers);
    parts.extend(diagnostics.iter().cloned());
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileStatus;

    fn settings() -> ReviewSettings {
        ReviewSettings {
            github_token: "x".into(),
            api_key: "x".into(),
            provider: "openai".into(),
            model: "model".into(),
            embedding_model: "embed".into(),
            embedding_cache: None,
            max_files: 2,
            max_diff_chars: 20,
            max_file_chars: 12,
            batch_chars: 12,
            max_findings: 30,
            rag_context_chars: 6000,
        }
    }

    fn changed(path: &str, patch: Option<&str>) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            previous_path: None,
            status: FileStatus::Modified,
            additions: 1,
            deletions: 0,
            patch: patch.map(str::to_owned),
        }
    }

    #[test]
    fn selection_prioritizes_source_and_reports_partial_coverage() {
        let files = vec![
            changed("README.md", Some("docs")),
            changed("src/lib.rs", Some("123456789012345")),
            changed("Cargo.lock", Some("lock")),
        ];
        let (selected, coverage) = select_files(&files, &settings());
        assert_eq!(selected[0].path, "src/lib.rs");
        assert!(selected[0].truncated);
        assert!(coverage.partial);
        assert_eq!(coverage.skipped_files, 1);
    }

    #[test]
    fn batches_include_every_selected_file_once() {
        let files = vec![
            SelectedFile {
                path: "a.rs".into(),
                patch: "123456".into(),
                priority: FilePriority::Production,
                truncated: false,
            },
            SelectedFile {
                path: "b.rs".into(),
                patch: "abcdef".into(),
                priority: FilePriority::Production,
                truncated: false,
            },
        ];
        let paths = build_batches(&files, 6)
            .into_iter()
            .flatten()
            .map(|file| file.path)
            .collect::<Vec<_>>();
        assert_eq!(paths, ["a.rs", "b.rs"]);
    }

    #[test]
    fn path_classification_is_stable_for_supported_and_excluded_files() {
        let cases = [
            ("src/lib.rs", Some(FilePriority::Production)),
            ("Cargo.toml", Some(FilePriority::Configuration)),
            ("tests/review.rs", Some(FilePriority::Test)),
            ("docs/review.md", Some(FilePriority::Documentation)),
            ("vendor/lib.rs", None),
            ("src/api.generated.rs", None),
            ("web/app.min.js", None),
            ("Cargo.lock", None),
            ("assets/logo.png", None),
        ];
        for (path, expected) in cases {
            assert_eq!(classify_path(path), expected, "{path}");
        }
    }

    #[test]
    fn patch_limit_is_unicode_safe_and_reports_truncation() {
        let patch = format!("@@ -0,0 +1 @@\n+{}", "🙂".repeat(20));
        let files = vec![changed("src/lib.rs", Some(&patch))];
        let (selected, coverage) = select_files(&files, &settings());
        assert!(selected[0].patch.is_char_boundary(selected[0].patch.len()));
        assert!(selected[0].patch.chars().count() <= 12);
        assert!(coverage.notes.iter().any(|note| note.contains("усечён")));
    }

    #[test]
    fn retrieval_query_adds_only_explicit_diagnostics() {
        let files = vec![SelectedFile {
            path: "src/lib.rs".into(),
            patch: "@@ -1 +1 @@\n-old\n+new".into(),
            priority: FilePriority::Production,
            truncated: false,
        }];
        let plain = retrieval_query(&files, &[]);
        let failed = retrieval_query(&files, &["tests::fails".into(), "E0308".into()]);
        assert!(!plain.contains("tests::fails"));
        assert!(plain.contains("old"));
        assert!(plain.contains("new"));
        assert!(failed.ends_with("tests::fails\nE0308"));
    }
}
