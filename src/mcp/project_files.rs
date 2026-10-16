use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use similar::TextDiff;
use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) const MAX_PROJECT_PATH_CHARS: usize = 1024;
pub(crate) const MAX_PROJECT_FILE_BYTES: usize = 256 * 1024;
pub(crate) const MAX_PROJECT_READ_BYTES: usize = 64 * 1024;
pub(crate) const MAX_PROJECT_FILES: usize = 10_000;
pub(crate) const MAX_PROJECT_SEARCH_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_PROJECT_MATCHES: usize = 200;
pub(crate) const MAX_PROJECT_SNIPPET_CHARS: usize = 400;
pub(crate) const MAX_PROJECT_OUTPUT_BYTES: usize = 64 * 1024;
const PROJECT_PROCESS_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListProjectFilesRequest {
    #[serde(default)]
    pub(crate) prefix: Option<String>,
    #[serde(default)]
    pub(crate) extensions: Vec<String>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct ListProjectFilesResult {
    pub(crate) files: Vec<String>,
    pub(crate) truncated: bool,
    pub(crate) truncation_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchProjectFilesRequest {
    pub(crate) query: String,
    #[serde(default)]
    pub(crate) prefix: Option<String>,
    #[serde(default)]
    pub(crate) extensions: Vec<String>,
    #[serde(default)]
    pub(crate) max_results: Option<usize>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct ProjectSearchMatch {
    pub(crate) path: String,
    pub(crate) line: usize,
    pub(crate) snippet: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct SearchProjectFilesResult {
    pub(crate) matches: Vec<ProjectSearchMatch>,
    pub(crate) files_scanned: usize,
    pub(crate) bytes_scanned: usize,
    pub(crate) truncated: bool,
    pub(crate) truncation_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadProjectFileRequest {
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) start_line: Option<usize>,
    #[serde(default)]
    pub(crate) end_line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct ReadProjectFileResult {
    pub(crate) path: String,
    pub(crate) content: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) total_lines: usize,
    pub(crate) bytes: usize,
    pub(crate) sha256: String,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProjectWriteMode {
    Create,
    Replace,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteProjectFileRequest {
    pub(crate) path: String,
    pub(crate) mode: ProjectWriteMode,
    pub(crate) content: String,
    #[serde(default)]
    pub(crate) expected_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PreparedProjectWrite {
    pub(crate) path: String,
    pub(crate) mode: ProjectWriteMode,
    pub(crate) old_sha256: Option<String>,
    pub(crate) new_sha256: String,
    pub(crate) bytes: usize,
    pub(crate) changed: bool,
    pub(crate) diff: String,
    pub(crate) diff_truncated: bool,
    #[serde(skip)]
    target: PathBuf,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct WriteProjectFileResult {
    pub(crate) path: String,
    pub(crate) mode: ProjectWriteMode,
    pub(crate) changed: bool,
    pub(crate) old_sha256: Option<String>,
    pub(crate) new_sha256: String,
    pub(crate) bytes_written: usize,
    pub(crate) diff: String,
    pub(crate) diff_truncated: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectGitDiffRequest {
    #[serde(default)]
    pub(crate) paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct ProjectGitDiffResult {
    pub(crate) diff: String,
    pub(crate) truncated: bool,
    pub(crate) truncation_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectWorkspace {
    policy: ProjectWorkspacePolicy,
}

#[derive(Debug, Clone)]
struct ProjectWorkspacePolicy {
    root: PathBuf,
}

impl ProjectWorkspacePolicy {
    fn new(root: PathBuf) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("не удалось определить workspace {}", root.display()))?;
        anyhow::ensure!(root.is_dir(), "workspace не является каталогом");
        Ok(Self { root })
    }

    fn relative_path(&self, raw: &str) -> Result<(String, PathBuf)> {
        anyhow::ensure!(!raw.trim().is_empty(), "путь не может быть пустым");
        anyhow::ensure!(
            raw.chars().count() <= MAX_PROJECT_PATH_CHARS,
            "путь превышает лимит"
        );
        anyhow::ensure!(!raw.contains('\0'), "путь содержит недопустимый символ");
        let path = Path::new(raw);
        anyhow::ensure!(!path.is_absolute(), "путь должен быть относительным");
        let mut clean = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(value) => clean.push(value),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    bail!("путь выходит за пределы workspace")
                }
            }
        }
        anyhow::ensure!(!clean.as_os_str().is_empty(), "путь не может быть пустым");
        anyhow::ensure!(
            !is_denied_path(&clean),
            "доступ к пути запрещён политикой workspace"
        );
        let relative = clean.to_string_lossy().replace('\\', "/");
        Ok((relative, self.root.join(clean)))
    }

    fn existing_file(&self, raw: &str) -> Result<(String, PathBuf, fs::Metadata)> {
        let (relative, candidate) = self.relative_path(raw)?;
        let metadata = fs::symlink_metadata(&candidate)
            .with_context(|| format!("project file недоступен: {relative}"))?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "символические ссылки запрещены"
        );
        anyhow::ensure!(metadata.is_file(), "цель не является обычным файлом");
        anyhow::ensure!(
            metadata.len() <= MAX_PROJECT_FILE_BYTES as u64,
            "project file превышает лимит {MAX_PROJECT_FILE_BYTES} байт"
        );
        let canonical = candidate
            .canonicalize()
            .context("не удалось канонизировать project file")?;
        anyhow::ensure!(
            canonical.starts_with(&self.root),
            "путь выходит за пределы workspace"
        );
        Ok((relative, canonical, metadata))
    }

    fn create_target(&self, raw: &str) -> Result<(String, PathBuf)> {
        let (relative, target) = self.relative_path(raw)?;
        let parent = target
            .parent()
            .context("у project file нет родительского каталога")?;
        let parent_metadata = fs::symlink_metadata(parent)
            .context("родительский каталог project file не существует")?;
        anyhow::ensure!(
            !parent_metadata.file_type().is_symlink(),
            "родительская символическая ссылка запрещена"
        );
        anyhow::ensure!(
            parent_metadata.is_dir(),
            "родитель project file не является каталогом"
        );
        let canonical_parent = parent
            .canonicalize()
            .context("не удалось канонизировать родительский каталог")?;
        anyhow::ensure!(
            canonical_parent.starts_with(&self.root),
            "путь выходит за пределы workspace"
        );
        Ok((relative, target))
    }
}

impl ProjectWorkspace {
    pub(crate) fn new(root: PathBuf) -> Result<Self> {
        Ok(Self {
            policy: ProjectWorkspacePolicy::new(root)?,
        })
    }

    pub(crate) fn list_files(
        &self,
        request: &ListProjectFilesRequest,
    ) -> Result<ListProjectFilesResult> {
        let prefix = self.validated_prefix(request.prefix.as_deref())?;
        let extensions = normalized_extensions(&request.extensions)?;
        let requested_limit = request.limit.unwrap_or(MAX_PROJECT_FILES);
        anyhow::ensure!(requested_limit > 0, "limit должен быть положительным");
        let limit = requested_limit.min(MAX_PROJECT_FILES);
        let (mut files, walk_truncated) = self.walk_files(prefix.as_deref(), &extensions)?;
        let truncated = walk_truncated || files.len() > limit;
        files.truncate(limit);
        Ok(ListProjectFilesResult {
            files,
            truncated,
            truncation_reason: truncated.then(|| {
                if walk_truncated {
                    "file_limit".to_owned()
                } else {
                    "result_limit".to_owned()
                }
            }),
        })
    }

    pub(crate) fn search_files(
        &self,
        request: &SearchProjectFilesRequest,
    ) -> Result<SearchProjectFilesResult> {
        anyhow::ensure!(
            !request.query.is_empty(),
            "строка поиска не может быть пустой"
        );
        anyhow::ensure!(
            request.query.chars().count() <= 4096,
            "строка поиска слишком длинная"
        );
        let prefix = self.validated_prefix(request.prefix.as_deref())?;
        let extensions = normalized_extensions(&request.extensions)?;
        let requested_limit = request.max_results.unwrap_or(MAX_PROJECT_MATCHES);
        anyhow::ensure!(requested_limit > 0, "max_results должен быть положительным");
        let match_limit = requested_limit.min(MAX_PROJECT_MATCHES);
        let (files, walk_truncated) = self.walk_files(prefix.as_deref(), &extensions)?;
        let mut matches = Vec::new();
        let mut files_scanned = 0usize;
        let mut bytes_scanned = 0usize;
        let mut reason = walk_truncated.then(|| "file_limit".to_owned());
        'files: for relative in files {
            let (_, path, metadata) = match self.policy.existing_file(&relative) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let size = metadata.len() as usize;
            if bytes_scanned.saturating_add(size) > MAX_PROJECT_SEARCH_BYTES {
                reason = Some("byte_limit".into());
                break;
            }
            let bytes = fs::read(path)?;
            let Ok(content) = std::str::from_utf8(&bytes) else {
                continue;
            };
            files_scanned += 1;
            bytes_scanned += bytes.len();
            for (index, line) in content.lines().enumerate() {
                if line.contains(&request.query) {
                    matches.push(ProjectSearchMatch {
                        path: relative.clone(),
                        line: index + 1,
                        snippet: truncate_chars(line.trim(), MAX_PROJECT_SNIPPET_CHARS),
                    });
                    if matches.len() >= match_limit {
                        reason = Some("match_limit".into());
                        break 'files;
                    }
                    if serialized_size(&matches) > MAX_PROJECT_OUTPUT_BYTES {
                        matches.pop();
                        reason = Some("output_limit".into());
                        break 'files;
                    }
                }
            }
        }
        Ok(SearchProjectFilesResult {
            matches,
            files_scanned,
            bytes_scanned,
            truncated: reason.is_some(),
            truncation_reason: reason,
        })
    }

    pub(crate) fn read_file(
        &self,
        request: &ReadProjectFileRequest,
    ) -> Result<ReadProjectFileResult> {
        let (relative, path, metadata) = self.policy.existing_file(&request.path)?;
        let bytes = fs::read(path).context("не удалось прочитать project file")?;
        let content = std::str::from_utf8(&bytes).context("project file не является UTF-8")?;
        let lines = content.split_inclusive('\n').collect::<Vec<_>>();
        let total_lines = lines.len();
        let start = request.start_line.unwrap_or(1);
        let end = request.end_line.unwrap_or(total_lines.max(1));
        anyhow::ensure!(start > 0, "start_line должен быть не меньше 1");
        anyhow::ensure!(end >= start, "end_line должен быть не меньше start_line");
        let start_index = start.saturating_sub(1).min(total_lines);
        let end_index = end.min(total_lines);
        let selected = if start_index < end_index {
            lines[start_index..end_index].concat()
        } else {
            String::new()
        };
        let (selected, truncated) = truncate_bytes(&selected, MAX_PROJECT_READ_BYTES);
        Ok(ReadProjectFileResult {
            path: relative,
            content: selected,
            start_line: start_index.saturating_add(1),
            end_line: end_index,
            total_lines,
            bytes: metadata.len() as usize,
            sha256: sha256_hex(&bytes),
            truncated,
        })
    }

    pub(crate) fn prepare_write(
        &self,
        request: &WriteProjectFileRequest,
    ) -> Result<PreparedProjectWrite> {
        anyhow::ensure!(
            request.content.len() <= MAX_PROJECT_FILE_BYTES,
            "содержимое превышает лимит {MAX_PROJECT_FILE_BYTES} байт"
        );
        let new_sha256 = sha256_hex(request.content.as_bytes());
        let (path, target, old_content, old_sha256) = match request.mode {
            ProjectWriteMode::Create => {
                anyhow::ensure!(
                    request.expected_sha256.is_none(),
                    "create не принимает expected_sha256"
                );
                let (path, target) = self.policy.create_target(&request.path)?;
                anyhow::ensure!(!target.exists(), "conflict: цель create уже существует");
                (path, target, String::new(), None)
            }
            ProjectWriteMode::Replace => {
                let expected = request
                    .expected_sha256
                    .as_deref()
                    .context("replace требует expected_sha256")?;
                anyhow::ensure!(
                    valid_sha256(expected),
                    "expected_sha256 должен быть lowercase SHA-256"
                );
                let (path, target, _) = self.policy.existing_file(&request.path)?;
                let bytes = fs::read(&target)?;
                let content =
                    String::from_utf8(bytes.clone()).context("project file не является UTF-8")?;
                let actual = sha256_hex(&bytes);
                anyhow::ensure!(
                    actual == expected,
                    "conflict: project file изменился после чтения"
                );
                (path, target, content, Some(actual))
            }
        };
        let changed = old_content.as_bytes() != request.content.as_bytes();
        let old_label = if request.mode == ProjectWriteMode::Create {
            "/dev/null".to_owned()
        } else {
            format!("a/{path}")
        };
        let new_label = format!("b/{path}");
        let raw_diff = if changed {
            TextDiff::from_lines(&old_content, &request.content)
                .unified_diff()
                .context_radius(3)
                .header(&old_label, &new_label)
                .to_string()
        } else {
            String::new()
        };
        let (diff, diff_truncated) = truncate_bytes(&raw_diff, MAX_PROJECT_OUTPUT_BYTES);
        Ok(PreparedProjectWrite {
            path,
            mode: request.mode,
            old_sha256,
            new_sha256,
            bytes: request.content.len(),
            changed,
            diff,
            diff_truncated,
            target,
        })
    }

    pub(crate) fn write_file(
        &self,
        request: &WriteProjectFileRequest,
    ) -> Result<WriteProjectFileResult> {
        let prepared = self.prepare_write(request)?;
        if !prepared.changed {
            return Ok(write_result(&prepared, 0));
        }
        let parent = prepared
            .target
            .parent()
            .context("нет родительского каталога")?;
        let temp = parent.join(format!(".fox-project-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            let mut file = options
                .open(&temp)
                .context("не удалось создать временный project file")?;
            file.write_all(request.content.as_bytes())
                .context("не удалось записать временный project file")?;
            file.sync_all()
                .context("не удалось синхронизировать project file")?;
            if request.mode == ProjectWriteMode::Replace {
                let permissions = fs::metadata(&prepared.target)?.permissions();
                fs::set_permissions(&temp, permissions)?;
            }
            let rechecked = self.prepare_write(request)?;
            match request.mode {
                ProjectWriteMode::Create => {
                    fs::hard_link(&temp, &prepared.target).map_err(|error| {
                        anyhow::anyhow!("conflict: цель create появилась: {error}")
                    })?;
                    fs::remove_file(&temp)?;
                }
                ProjectWriteMode::Replace => {
                    fs::rename(&temp, &prepared.target)
                        .context("не удалось атомарно заменить project file")?;
                }
            }
            sync_parent(parent)?;
            Ok(write_result(&rechecked, request.content.len()))
        })();
        if temp.exists() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    pub(crate) fn git_diff(&self, request: &ProjectGitDiffRequest) -> Result<ProjectGitDiffResult> {
        let requested_paths = if request.paths.is_empty() {
            None
        } else {
            Some(
                request
                    .paths
                    .iter()
                    .map(|path| {
                        self.policy
                            .relative_path(path)
                            .map(|(relative, _)| relative)
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        };
        let paths = match &requested_paths {
            Some(paths) => paths.clone(),
            None => self.changed_tracked_paths()?,
        };
        let mut arguments = vec![
            OsString::from("diff"),
            OsString::from("--no-ext-diff"),
            OsString::from("--no-textconv"),
            OsString::from("--"),
        ];
        arguments.extend(paths.iter().map(OsString::from));
        let mut diff = if paths.is_empty() {
            String::new()
        } else {
            let tracked = run_bounded_command(
                self.policy.root.clone(),
                OsString::from("git"),
                arguments,
                PROJECT_PROCESS_TIMEOUT,
                MAX_PROJECT_OUTPUT_BYTES,
            )?;
            anyhow::ensure!(tracked.success, "git diff завершился с ошибкой");
            let diff = String::from_utf8(tracked.stdout).context("git diff вернул не-UTF-8")?;
            if tracked.truncated {
                return Ok(ProjectGitDiffResult {
                    diff,
                    truncated: true,
                    truncation_reason: Some("output_limit".into()),
                });
            }
            diff
        };
        let untracked = self.untracked_paths(requested_paths.as_deref().unwrap_or_default())?;
        for path in untracked {
            let (_, absolute, _) = self.policy.existing_file(&path)?;
            let content =
                fs::read_to_string(absolute).context("untracked file не является UTF-8")?;
            let addition = TextDiff::from_lines("", &content)
                .unified_diff()
                .context_radius(3)
                .header("/dev/null", &format!("b/{path}"))
                .to_string();
            diff.push_str(&addition);
            if diff.len() > MAX_PROJECT_OUTPUT_BYTES {
                let (diff, _) = truncate_bytes(&diff, MAX_PROJECT_OUTPUT_BYTES);
                return Ok(ProjectGitDiffResult {
                    diff,
                    truncated: true,
                    truncation_reason: Some("output_limit".into()),
                });
            }
        }
        Ok(ProjectGitDiffResult {
            diff,
            truncated: false,
            truncation_reason: None,
        })
    }

    fn validated_prefix(&self, prefix: Option<&str>) -> Result<Option<String>> {
        prefix
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                self.policy
                    .relative_path(value)
                    .map(|(relative, _)| relative)
            })
            .transpose()
    }

    fn changed_tracked_paths(&self) -> Result<Vec<String>> {
        let output = run_bounded_command(
            self.policy.root.clone(),
            OsString::from("git"),
            vec![
                OsString::from("diff"),
                OsString::from("--name-only"),
                OsString::from("-z"),
                OsString::from("--"),
            ],
            PROJECT_PROCESS_TIMEOUT,
            MAX_PROJECT_OUTPUT_BYTES,
        )?;
        anyhow::ensure!(output.success, "git diff --name-only завершился с ошибкой");
        anyhow::ensure!(
            !output.truncated,
            "список изменённых tracked files превысил лимит"
        );
        let stdout = String::from_utf8(output.stdout).context("Git path не является UTF-8")?;
        let mut paths = stdout
            .split('\0')
            .filter(|path| !path.is_empty())
            .filter_map(|path| self.policy.relative_path(path).ok().map(|value| value.0))
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    fn walk_files(
        &self,
        prefix: Option<&str>,
        extensions: &[String],
    ) -> Result<(Vec<String>, bool)> {
        let root = self.policy.root.clone();
        let policy = self.policy.clone();
        let mut builder = WalkBuilder::new(&root);
        builder
            .hidden(false)
            .follow_links(false)
            .standard_filters(true);
        builder.filter_entry(move |entry| {
            entry
                .path()
                .strip_prefix(&policy.root)
                .map(|relative| relative.as_os_str().is_empty() || !is_denied_path(relative))
                .unwrap_or(false)
        });
        let mut files = Vec::new();
        let mut truncated = false;
        for entry in builder.build() {
            let Ok(entry) = entry else { continue };
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&root)
                .context("walk path вне workspace")?
                .to_string_lossy()
                .replace('\\', "/");
            if prefix.is_some_and(|prefix| {
                relative != prefix && !relative.starts_with(&format!("{prefix}/"))
            }) {
                continue;
            }
            if !extensions.is_empty() && !path_has_extension(&relative, extensions) {
                continue;
            }
            files.push(relative);
            if files.len() > MAX_PROJECT_FILES {
                truncated = true;
                break;
            }
        }
        files.sort();
        files.truncate(MAX_PROJECT_FILES);
        Ok((files, truncated))
    }

    fn untracked_paths(&self, requested: &[String]) -> Result<Vec<String>> {
        let output = run_bounded_command(
            self.policy.root.clone(),
            OsString::from("git"),
            vec![
                OsString::from("ls-files"),
                OsString::from("--others"),
                OsString::from("--exclude-standard"),
            ],
            PROJECT_PROCESS_TIMEOUT,
            MAX_PROJECT_OUTPUT_BYTES,
        )?;
        anyhow::ensure!(output.success, "git ls-files завершился с ошибкой");
        anyhow::ensure!(
            !output.truncated,
            "список untracked files превысил лимит вывода"
        );
        let stdout = String::from_utf8(output.stdout).context("git ls-files вернул не-UTF-8")?;
        let mut values = stdout
            .lines()
            .filter(|path| {
                !is_denied_path(Path::new(path))
                    && (requested.is_empty() || requested.iter().any(|value| value == path))
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        values.sort();
        Ok(values)
    }
}

pub(crate) fn prepare_project_write_preview(
    root: &Path,
    arguments: &serde_json::Value,
) -> Result<PreparedProjectWrite> {
    let request: WriteProjectFileRequest =
        serde_json::from_value(arguments.clone()).context("аргументы project write некорректны")?;
    ProjectWorkspace::new(root.to_path_buf())?.prepare_write(&request)
}

fn write_result(prepared: &PreparedProjectWrite, bytes_written: usize) -> WriteProjectFileResult {
    WriteProjectFileResult {
        path: prepared.path.clone(),
        mode: prepared.mode,
        changed: prepared.changed,
        old_sha256: prepared.old_sha256.clone(),
        new_sha256: prepared.new_sha256.clone(),
        bytes_written,
        diff: prepared.diff.clone(),
        diff_truncated: prepared.diff_truncated,
    }
}

fn normalized_extensions(values: &[String]) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for value in values {
        let clean = value.trim().trim_start_matches('.').to_ascii_lowercase();
        anyhow::ensure!(
            !clean.is_empty()
                && clean
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()),
            "extension должна содержать только ASCII-буквы и цифры"
        );
        result.push(clean);
    }
    result.sort();
    result.dedup();
    Ok(result)
}

fn path_has_extension(path: &str, extensions: &[String]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| {
            extensions
                .iter()
                .any(|extension| value.eq_ignore_ascii_case(extension))
        })
        .unwrap_or(false)
}

fn is_denied_path(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(value) = component else {
            return true;
        };
        let name = value.to_string_lossy().to_ascii_lowercase();
        if name == ".env-mcp.example" {
            return false;
        }
        name == ".git"
            || name == "target"
            || name == ".env"
            || name == ".envrc"
            || name.starts_with(".env-")
            || name.starts_with(".env.")
            || name == ".fox-llm.json"
            || name == ".fox-embeddings.json"
            || name.starts_with(".fox-index.db")
            || name.starts_with(".fox-linkding-index.db")
            || name.starts_with(".fox-sessions.db")
            || name.starts_with(".fox-scheduler.db")
            || name.starts_with(".fox-support.db")
            || name == ".fox-rag-contract.log"
            || name == ".ai-review-cache"
            || name == "fox-metrics.log"
            || name == "mcp.log"
            || name == ".netrc"
            || name == ".npmrc"
            || name == "id_rsa"
            || name == "id_ed25519"
            || name.contains("credential")
            || name.starts_with("secret")
            || name.starts_with("token.")
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn truncate_bytes(value: &str, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value.to_owned(), false);
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn serialized_size<T: Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn sync_parent(parent: &Path) -> Result<()> {
    let directory = fs::File::open(parent).context("не удалось открыть родительский каталог")?;
    directory
        .sync_all()
        .context("не удалось синхронизировать родительский каталог")
}

struct BoundedProcessOutput {
    success: bool,
    stdout: Vec<u8>,
    truncated: bool,
}

fn run_bounded_command(
    root: PathBuf,
    program: OsString,
    arguments: Vec<OsString>,
    timeout: Duration,
    output_limit: usize,
) -> Result<BoundedProcessOutput> {
    let mut child = Command::new(program)
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().context("stdout процесса недоступен")?;
    let stderr = child.stderr.take().context("stderr процесса недоступен")?;
    let stdout_thread = std::thread::spawn(move || read_bounded(stdout, output_limit));
    let stderr_thread = std::thread::spawn(move || read_bounded(stderr, output_limit));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            bail!("процесс превысил тайм-аут");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("сбой чтения stdout"))??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("сбой чтения stderr"))??;
    Ok(BoundedProcessOutput {
        success: status.success(),
        stdout: stdout.bytes,
        truncated: stdout.truncated || stderr.truncated,
    })
}

struct BoundedRead {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Result<BoundedRead> {
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    Ok(BoundedRead { bytes, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, ProjectWorkspace) {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("src")).unwrap();
        fs::write(
            directory.path().join("src/api.rs"),
            "pub fn call_api() {}\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("src/app.rs"),
            "fn main() { call_api(); }\n",
        )
        .unwrap();
        fs::write(directory.path().join("README.md"), "# Demo\n").unwrap();
        let project = ProjectWorkspace::new(directory.path().to_path_buf()).unwrap();
        (directory, project)
    }

    #[test]
    fn policy_rejects_escape_secrets_binary_and_oversized_files() {
        let (directory, project) = workspace();
        fs::write(directory.path().join(".env-mcp"), "SECRET=x").unwrap();
        fs::write(directory.path().join("binary.bin"), [0xff, 0xfe]).unwrap();
        fs::write(
            directory.path().join("large.txt"),
            vec![b'x'; MAX_PROJECT_FILE_BYTES + 1],
        )
        .unwrap();
        for path in [
            "../outside",
            ".env-mcp",
            ".envrc",
            ".fox-embeddings.json",
            ".fox-rag-contract.log",
            "secrets.txt",
            "token.txt",
        ] {
            assert!(project
                .read_file(&ReadProjectFileRequest {
                    path: path.into(),
                    start_line: None,
                    end_line: None,
                })
                .is_err());
        }
        for path in ["binary.bin", "large.txt"] {
            assert!(project
                .read_file(&ReadProjectFileRequest {
                    path: path.into(),
                    start_line: None,
                    end_line: None,
                })
                .is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn policy_rejects_symlinks_even_when_target_is_outside() {
        use std::os::unix::fs::symlink;
        let (directory, project) = workspace();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), directory.path().join("escape.txt")).unwrap();
        assert!(project
            .read_file(&ReadProjectFileRequest {
                path: "escape.txt".into(),
                start_line: None,
                end_line: None,
            })
            .is_err());
    }

    #[test]
    fn list_and_search_are_stable_filtered_and_report_line_numbers() {
        let (directory, project) = workspace();
        fs::write(directory.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(directory.path().join("ignored.txt"), "call_api\n").unwrap();
        let listed = project
            .list_files(&ListProjectFilesRequest {
                prefix: Some("src".into()),
                extensions: vec!["rs".into()],
                limit: None,
            })
            .unwrap();
        assert_eq!(listed.files, ["src/api.rs", "src/app.rs"]);
        let found = project
            .search_files(&SearchProjectFilesRequest {
                query: "call_api".into(),
                prefix: None,
                extensions: vec![".rs".into()],
                max_results: None,
            })
            .unwrap();
        assert_eq!(found.matches.len(), 2);
        assert_eq!(found.matches[0].path, "src/api.rs");
        assert_eq!(found.matches[0].line, 1);
        assert_eq!(found.matches[1].path, "src/app.rs");
        assert!(project
            .read_file(&ReadProjectFileRequest {
                path: "ignored.txt".into(),
                start_line: None,
                end_line: None,
            })
            .is_ok());
        let limited = project
            .search_files(&SearchProjectFilesRequest {
                query: "call_api".into(),
                prefix: None,
                extensions: vec!["rs".into()],
                max_results: Some(1),
            })
            .unwrap();
        assert_eq!(limited.matches.len(), 1);
        assert_eq!(limited.truncation_reason.as_deref(), Some("match_limit"));
    }

    #[test]
    fn read_returns_range_unicode_and_full_file_hash() {
        let (directory, project) = workspace();
        fs::write(directory.path().join("unicode.txt"), "ёж\nдва\nтри\n").unwrap();
        let result = project
            .read_file(&ReadProjectFileRequest {
                path: "unicode.txt".into(),
                start_line: Some(2),
                end_line: Some(3),
            })
            .unwrap();
        assert_eq!(result.content, "два\nтри\n");
        assert_eq!(result.total_lines, 3);
        assert_eq!(result.sha256, sha256_hex("ёж\nдва\nтри\n".as_bytes()));

        let long = "ёж".repeat(MAX_PROJECT_READ_BYTES);
        fs::write(directory.path().join("long.txt"), &long).unwrap();
        let result = project
            .read_file(&ReadProjectFileRequest {
                path: "long.txt".into(),
                start_line: None,
                end_line: None,
            })
            .unwrap();
        assert!(result.truncated);
        assert!(std::str::from_utf8(result.content.as_bytes()).is_ok());

        fs::write(directory.path().join("exact.txt"), "one\r\ntwo\r\n").unwrap();
        let result = project
            .read_file(&ReadProjectFileRequest {
                path: "exact.txt".into(),
                start_line: None,
                end_line: None,
            })
            .unwrap();
        assert_eq!(result.content, "one\r\ntwo\r\n");
    }

    #[test]
    fn create_replace_noop_and_stale_hash_are_safe() {
        let (directory, project) = workspace();
        let create = WriteProjectFileRequest {
            path: "NEW.md".into(),
            mode: ProjectWriteMode::Create,
            content: "new\n".into(),
            expected_sha256: None,
        };
        let preview = project.prepare_write(&create).unwrap();
        assert!(preview.diff.contains("/dev/null"));
        fs::write(directory.path().join("NEW.md"), "concurrent\n").unwrap();
        assert!(project.write_file(&create).is_err());
        assert_eq!(
            fs::read_to_string(directory.path().join("NEW.md")).unwrap(),
            "concurrent\n"
        );
        fs::remove_file(directory.path().join("NEW.md")).unwrap();
        assert!(project.write_file(&create).unwrap().changed);
        let hash = sha256_hex(b"new\n");
        let replace = WriteProjectFileRequest {
            path: "NEW.md".into(),
            mode: ProjectWriteMode::Replace,
            content: "updated\n".into(),
            expected_sha256: Some(hash.clone()),
        };
        assert!(project.write_file(&replace).unwrap().changed);
        assert_eq!(
            fs::read_to_string(directory.path().join("NEW.md")).unwrap(),
            "updated\n"
        );
        assert!(project.write_file(&replace).is_err());
        let current = sha256_hex(b"updated\n");
        let noop = WriteProjectFileRequest {
            content: "updated\n".into(),
            expected_sha256: Some(current),
            ..replace
        };
        let result = project.write_file(&noop).unwrap();
        assert!(!result.changed);
        assert_eq!(result.bytes_written, 0);
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".fox-project-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn replace_preserves_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let (directory, project) = workspace();
        let target = directory.path().join("script.sh");
        fs::write(&target, "old\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        project
            .write_file(&WriteProjectFileRequest {
                path: "script.sh".into(),
                mode: ProjectWriteMode::Replace,
                content: "new\n".into(),
                expected_sha256: Some(sha256_hex(b"old\n")),
            })
            .unwrap();
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn git_diff_includes_tracked_and_untracked_text() {
        let (directory, project) = workspace();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(directory.path())
            .status()
            .unwrap();
        fs::write(directory.path().join(".env"), "SECRET=before\n").unwrap();
        Command::new("git")
            .args(["add", "-f", ".env"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "base",
            ])
            .current_dir(directory.path())
            .status()
            .unwrap();
        fs::write(directory.path().join("README.md"), "# Changed\n").unwrap();
        fs::write(directory.path().join("NEW.md"), "new\n").unwrap();
        fs::write(directory.path().join(".env"), "SECRET=after\n").unwrap();
        let result = project
            .git_diff(&ProjectGitDiffRequest { paths: Vec::new() })
            .unwrap();
        assert!(result.diff.contains("README.md"));
        assert!(result.diff.contains("NEW.md"));
        assert!(!result.diff.contains("SECRET"));
        assert!(!result.diff.contains(".env"));
    }

    #[test]
    fn git_diff_reports_output_truncation() {
        let (directory, project) = workspace();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(directory.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "base",
            ])
            .current_dir(directory.path())
            .status()
            .unwrap();
        fs::write(
            directory.path().join("README.md"),
            "changed line\n".repeat(MAX_PROJECT_OUTPUT_BYTES),
        )
        .unwrap();
        let result = project
            .git_diff(&ProjectGitDiffRequest { paths: Vec::new() })
            .unwrap();
        assert!(result.truncated);
        assert_eq!(result.truncation_reason.as_deref(), Some("output_limit"));
        assert!(result.diff.len() <= MAX_PROJECT_OUTPUT_BYTES);
    }
}
