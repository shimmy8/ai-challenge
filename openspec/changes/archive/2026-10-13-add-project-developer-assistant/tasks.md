## 1. Project Corpus and Retrieval Filtering

- [x] 1.1 Add regression tests for default document discovery to require `README.md`, `reports/**/*.md` and `openspec/specs/**/*.md`, then verify the focused document tests fail before and pass after the discovery change.
- [x] 1.2 Add a source allowlist/predicate to retrieval and tests proving project retrieval includes only `README.md` and `openspec/specs/**` even when reports and archived changes rank higher.
- [x] 1.3 Preserve ordinary RAG retrieval without a source filter and verify the existing RAG retrieval and indexing test suites still pass.

## 2. Read-only Workspace MCP Tools

- [x] 2.1 Add `src/mcp/workspace.rs` with testable process abstractions and failing tests for named branch, detached HEAD and non-repository Git outcomes.
- [x] 2.2 Implement `git_current_branch` with fixed arguments and workspace cwd, register it on `WorkspaceServer` as read-only, and verify MCP discovery plus temporary-repository tests pass.
- [x] 2.3 Add failing tests for `codegraph_explore` input validation, missing `.codegraph/`, missing executable, argv-safe question passing, timeout, process reap and bounded output.
- [x] 2.4 Implement constrained CodeGraph execution without a shell, with canonical workspace cwd, timeout, termination/reap and output limits; register the tool as read-only and verify all focused workspace MCP tests pass.
- [x] 2.5 Add integration coverage proving the workspace server still exposes `save_report`, advertises both new tools with read-only annotations and does not expose unrelated server tools.

## 3. Developer Help Coordination

- [x] 3.1 Add parsing and output tests for exact `/help`, `/help <вопрос>` and whitespace-only input, including the dedicated project-question line in ordinary help.
- [x] 3.2 Introduce a project-help coordinator that accepts retrieval and `SharedToolExecutor` dependencies, resolves the required workspace tools, and independently records document, Git and CodeGraph outcomes; verify it with fake retrieval and MCP executors.
- [x] 3.3 Normalize successful MCP results into bounded named context sources and add tests that grounded synthesis can cite documentation, `mcp://workspace/git_current_branch` and `mcp://workspace/codegraph_explore` without persisting tool traces.
- [x] 3.4 Implement partial-failure behavior and tests: either MCP tool may fail while remaining sources answer, CodeGraph-only context may answer, and a fully empty context skips generation with a Russian diagnostic.
- [x] 3.5 Wire `/help <вопрос>` into the interactive loop independently of `rag_enabled`, preserve `/help` without arguments, save the stripped question and successful answer in history, and verify command-level regression tests.

## 4. Documentation and Validation

- [x] 4.1 Update `README.md` with default project indexing, workspace MCP startup/registration, CodeGraph prerequisites, `/help` examples and fallback behavior; verify all documented commands match the implemented CLI.
- [x] 4.2 Run `cargo fmt --all -- --check` and fix any formatting differences.
- [x] 4.3 Run `cargo clippy --all-targets --all-features -- -D warnings` and resolve all warnings.
- [x] 4.4 Run `cargo test` and confirm the complete regression suite passes without reading local secret files.

## 5. Isolated Help Subagent and Embedded MCP

- [x] 5.1 Add lifecycle and discovery tests for an automatically started help-only loopback MCP, proving that it binds a free port, exposes only `git_current_branch` and `codegraph_explore`, never exposes `save_report`, and shuts down cleanly.
- [x] 5.2 Implement the managed help-only MCP runtime in `src/mcp/`, start it with interactive `fox-llm`, keep it out of `.fox-llm.json`, and verify startup failure degrades to documentation-only project help.
- [x] 5.3 Add regression tests proving two sequential `/help` questions and a populated parent agent state do not influence the second query rewrite, tool calls, retrieved context or answer.
- [x] 5.4 Replace project-help generation with a fresh temporary agent per question, give it only the stripped question, project RAG chunks and the help-only MCP executor, validate an internal evidence envelope, render only clean answer text, and persist only the stripped question plus clean verified answer to the parent history; regression tests SHALL reject visible source/citation sections, `chunk_id`, MCP URI and tool trace.
- [x] 5.5 Update `README.md` to remove the manual workspace-MCP prerequisite for `/help`, document automatic startup, isolation and the citation-free user-facing workflow, and verify the demo commands match the implemented lifecycle.
- [x] 5.6 Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test` and `openspec validate add-project-developer-assistant --strict` after the isolation changes.
- [x] 5.7 Suppress server-side `mcp_server_call_*` stdout events for the internal `project-help` MCP while preserving logging for ordinary MCP servers, and cover the routing decision with a regression test.
- [x] 5.8 Wrap displayed agent answers at terminal word boundaries without changing persisted text or fenced code blocks, and cover Russian prose plus multiline preservation with regression tests.
- [x] 5.9 Bound help-only MCP graceful shutdown to 200 ms, abort and reap a stalled server task after the deadline, and add a regression test for the timeout path.
- [x] 5.10 Make clean help rendering accept ordinary prose containing «источники», strip standalone attribution sections, redact technical MCP identifiers, and retain rejection of a raw evidence envelope with regression coverage.
