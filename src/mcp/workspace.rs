use anyhow::{bail, Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_OUTPUT_LIMIT: usize = 32 * 1024;
const MAX_CODEGRAPH_QUESTION_CHARS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct GitBranchResult {
    pub(crate) branch: Option<String>,
    pub(crate) detached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct CodeGraphExploreResult {
    pub(crate) output: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeGraphExploreRequest {
    pub(crate) question: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceInspector {
    root: PathBuf,
    codegraph_program: OsString,
    timeout: Duration,
    output_limit: usize,
}

struct ProcessOutput {
    success: bool,
    stdout: Vec<u8>,
}

impl WorkspaceInspector {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            codegraph_program: OsString::from("codegraph"),
            timeout: DEFAULT_PROCESS_TIMEOUT,
            output_limit: DEFAULT_OUTPUT_LIMIT,
        }
    }

    #[cfg(test)]
    fn for_test(
        root: PathBuf,
        codegraph_program: PathBuf,
        timeout: Duration,
        output_limit: usize,
    ) -> Self {
        Self {
            root,
            codegraph_program: codegraph_program.into_os_string(),
            timeout,
            output_limit,
        }
    }

    pub(crate) async fn git_current_branch(&self) -> Result<GitBranchResult> {
        let branch = self
            .run(
                "git".as_ref(),
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
            )
            .await
            .context("не удалось запустить Git")?;
        if branch.success {
            let branch = String::from_utf8(branch.stdout)
                .context("Git вернул некорректное имя ветки")?
                .trim()
                .to_owned();
            anyhow::ensure!(!branch.is_empty(), "Git вернул пустое имя ветки");
            return Ok(GitBranchResult {
                branch: Some(branch),
                detached: false,
            });
        }

        let repository = self
            .run("git".as_ref(), &["rev-parse", "--is-inside-work-tree"])
            .await
            .context("не удалось проверить Git workspace")?;
        if repository.success && repository.stdout == b"true\n" {
            Ok(GitBranchResult {
                branch: None,
                detached: true,
            })
        } else {
            bail!("workspace не является Git-репозиторием")
        }
    }

    pub(crate) async fn codegraph_explore(&self, question: &str) -> Result<CodeGraphExploreResult> {
        let question = question.trim();
        anyhow::ensure!(
            !question.is_empty(),
            "вопрос CodeGraph не может быть пустым"
        );
        anyhow::ensure!(
            question.chars().count() <= MAX_CODEGRAPH_QUESTION_CHARS,
            "вопрос CodeGraph превышает лимит {MAX_CODEGRAPH_QUESTION_CHARS} символов"
        );
        anyhow::ensure!(
            self.root.join(".codegraph").is_dir(),
            "индекс .codegraph отсутствует в workspace"
        );
        let output = self
            .run(&self.codegraph_program, &["explore", question])
            .await
            .map_err(|error| anyhow::anyhow!("не удалось запустить CodeGraph: {error:#}"))?;
        anyhow::ensure!(output.success, "CodeGraph завершился с ошибкой");
        let output = String::from_utf8(output.stdout).context("CodeGraph вернул не-UTF-8 вывод")?;
        anyhow::ensure!(!output.trim().is_empty(), "CodeGraph вернул пустой вывод");
        Ok(CodeGraphExploreResult { output })
    }

    async fn run(&self, program: &OsStr, arguments: &[&str]) -> Result<ProcessOutput> {
        let root = self
            .root
            .canonicalize()
            .with_context(|| format!("не удалось определить workspace {}", self.root.display()))?;
        let program = program.to_os_string();
        let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
        let timeout = self.timeout;
        let output_limit = self.output_limit;
        tokio::task::spawn_blocking(move || {
            run_bounded_process(root, program, arguments, timeout, output_limit)
        })
        .await
        .context("не удалось дождаться процесса")?
    }
}

fn run_bounded_process(
    root: PathBuf,
    program: OsString,
    arguments: Vec<OsString>,
    timeout: Duration,
    output_limit: usize,
) -> Result<ProcessOutput> {
    let mut child = Command::new(program)
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().context("stdout процесса недоступен")?;
    let stderr = child.stderr.take().context("stderr процесса недоступен")?;
    let read_limit = output_limit.saturating_add(1) as u64;
    let stdout_thread = std::thread::spawn(move || read_limited(stdout, read_limit));
    let stderr_thread = std::thread::spawn(move || read_limited(stderr, read_limit));
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
    anyhow::ensure!(
        stdout.len() <= output_limit && stderr.len() <= output_limit,
        "вывод процесса превысил лимит {output_limit} байт"
    );
    Ok(ProcessOutput {
        success: status.success(),
        stdout,
    })
}

fn read_limited(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command, time::Duration};

    fn git(directory: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(directory)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[tokio::test]
    async fn git_branch_reports_named_detached_and_non_repository_states() {
        let named = tempfile::tempdir().unwrap();
        git(named.path(), &["init", "-q"]);
        git(
            named.path(),
            &["symbolic-ref", "HEAD", "refs/heads/feature/help"],
        );
        let inspector = WorkspaceInspector::new(named.path().to_path_buf());
        assert_eq!(
            inspector.git_current_branch().await.unwrap(),
            GitBranchResult {
                branch: Some("feature/help".into()),
                detached: false,
            }
        );

        git(
            named.path(),
            &[
                "-c",
                "user.name=Fox Test",
                "-c",
                "user.email=fox@example.test",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
        );
        git(named.path(), &["checkout", "--detach", "-q"]);
        assert_eq!(
            inspector.git_current_branch().await.unwrap(),
            GitBranchResult {
                branch: None,
                detached: true,
            }
        );

        let outside = tempfile::tempdir().unwrap();
        assert!(WorkspaceInspector::new(outside.path().to_path_buf())
            .git_current_branch()
            .await
            .unwrap_err()
            .to_string()
            .contains("Git-репозиторием"));
    }

    #[cfg(unix)]
    fn executable(directory: &std::path::Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("fake-codegraph");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codegraph_passes_question_as_one_literal_argument() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(".codegraph")).unwrap();
        let program = executable(directory.path(), "printf '%s\\n%s' \"$1\" \"$2\"");
        let marker = directory.path().join("shell-expanded");
        let question = format!("where is help?; touch {}", marker.display());
        let inspector = WorkspaceInspector::for_test(
            directory.path().to_path_buf(),
            program,
            Duration::from_secs(1),
            1024,
        );

        let result = inspector.codegraph_explore(&question).await.unwrap();

        assert_eq!(result.output, format!("explore\n{question}"));
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codegraph_rejects_invalid_or_unavailable_inputs_before_use() {
        let directory = tempfile::tempdir().unwrap();
        let inspector = WorkspaceInspector::for_test(
            directory.path().to_path_buf(),
            directory.path().join("missing-codegraph"),
            Duration::from_millis(100),
            32,
        );
        assert!(inspector.codegraph_explore(" ").await.is_err());
        assert!(inspector
            .codegraph_explore(&"x".repeat(4097))
            .await
            .is_err());
        assert!(inspector
            .codegraph_explore("question")
            .await
            .unwrap_err()
            .to_string()
            .contains(".codegraph"));

        fs::create_dir(directory.path().join(".codegraph")).unwrap();
        assert!(inspector
            .codegraph_explore("question")
            .await
            .unwrap_err()
            .to_string()
            .contains("запустить CodeGraph"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codegraph_limits_output_and_kills_timed_out_process() {
        let oversized = tempfile::tempdir().unwrap();
        fs::create_dir(oversized.path().join(".codegraph")).unwrap();
        let noisy = executable(oversized.path(), "printf '123456789'");
        let inspector = WorkspaceInspector::for_test(
            oversized.path().to_path_buf(),
            noisy,
            Duration::from_secs(1),
            8,
        );
        assert!(inspector
            .codegraph_explore("question")
            .await
            .unwrap_err()
            .to_string()
            .contains("превысил лимит"));

        let timed = tempfile::tempdir().unwrap();
        fs::create_dir(timed.path().join(".codegraph")).unwrap();
        let marker = timed.path().join("finished");
        let slow = executable(
            timed.path(),
            &format!("sleep 1\ntouch '{}'", marker.display()),
        );
        let inspector = WorkspaceInspector::for_test(
            timed.path().to_path_buf(),
            slow,
            Duration::from_millis(50),
            1024,
        );
        assert!(inspector
            .codegraph_explore("question")
            .await
            .unwrap_err()
            .to_string()
            .contains("тайм-аут"));
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists(), "timed-out process kept running");
    }
}
