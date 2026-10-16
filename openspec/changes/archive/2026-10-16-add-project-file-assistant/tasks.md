## 1. Workspace policy and read-only primitives

- [x] 1.1 Add `ignore` and `similar` dependencies plus `src/mcp/project_files.rs` request/result skeletons and constants; verify `cargo check --all-targets --all-features` resolves the new module without warnings.
- [x] 1.2 Write failing tempfile tests for relative-path normalization, canonical-root confinement, denylisted secrets/generated data, direct ignored-file access, symlink escape, regular UTF-8 checks and size limits; implement `ProjectWorkspacePolicy` until those focused tests pass.
- [x] 1.3 Write failing tests for stable gitignore-aware `list_files` and bounded literal `search_files`, including multiple files, one-based line numbers, snippets and every truncation reason; implement both operations and verify the focused tests pass.
- [x] 1.4 Write failing tests for ranged `read_file`, full-version SHA-256, Unicode line handling and bounded output; implement reading and verify the focused tests pass.
- [x] 1.5 Write failing tests for filtered `git_diff`, available untracked UTF-8 additions, denylisted paths, timeout and output truncation; implement process-safe diff collection without shell interpretation and verify the focused tests pass.

## 2. Safe write and approval preview

- [x] 2.1 Write failing tests for `create`/`replace` schema validation, required absence/hash preconditions, identical-content no-op and deterministic bounded unified diff; implement shared `prepare_write` and verify the focused tests pass.
- [x] 2.2 Write failing tests for exact confirmed write, stale hash, target appearing after create preview, permissions preservation, same-directory temporary file cleanup and atomic replacement; implement `write_file` and verify no rejected/conflicting case changes target bytes or metadata.
- [x] 2.3 Add project-specific interactive approval rendering in `app.rs` that calls the shared preview helper and shows mode, normalized path, hashes, size, unified diff and truncation before yes/no; verify tests reject invalid raw arguments without prompting and execute exactly one approved call.
- [x] 2.4 Add regression tests for user rejection and a concurrent edit between preview and execution; verify the first never reaches the MCP handler and the second returns conflict without overwriting the concurrent bytes.

## 3. Project-only MCP runtime

- [x] 3.1 Register `ProjectWorkspaceServer` tools in `src/mcp/group_servers.rs` with `deny_unknown_fields`, exact read-only/destructive annotations and no shell/network/general workspace tools; verify `tools/list` returns only the six designed native tools.
- [x] 3.2 Add `src/mcp/project_runtime.rs` using an ephemeral loopback address and qualified `project__*` routes; verify a runtime integration test can list/call tools without changing `.fox-llm.json` or the user MCP registry.
- [x] 3.3 Implement graceful project runtime shutdown with the existing 200 ms budget and forced task cancellation fallback; verify the lifecycle test leaves no reachable listener or pending server task.
- [x] 3.4 Export the new runtime and filesystem APIs only through `src/mcp/mod.rs` and keep the existing `workspace` and help-only tool lists unchanged; verify their existing exact-tool-list tests still pass.

## 4. Goal-oriented project subagent

- [x] 4.1 Add `/project` parsing and `src/mcp/project_assistant.rs` orchestration with a fresh Agent, copied provider settings/request client/global invariants, disabled pipeline planner and no parent history, summary, memory, profile, task, RAG or user tools; verify isolation with two consecutive scripted goals containing sentinel parent data.
- [x] 4.2 Add project system instructions and trace validation requiring successful read-only research plus read/list evidence before a write; verify a model answer without project tool calls is rejected as unverified and a write without source/target evidence is not reported as completed.
- [x] 4.3 Derive checked/changed/cancelled paths and bounded diffs from actual tool results, append them deterministically to the visible summary and keep technical traces out of persisted dialogue; verify a synthesis failure after a successful write still reports the completed path and diff separately from the provider error.
- [x] 4.4 Add the scripted API-usage scenario over at least `src/api.rs`, `src/app.rs` and `README.md`; verify search returns both matching files with line numbers, at least one file is read, approval is never called and every fixture byte remains unchanged.
- [x] 4.5 Add the scripted documentation-update scenario with code/README reads, one approved hash-bound write and final diff, then repeat it against the resulting workspace; verify the first run changes the expected Markdown and the second performs no write or returns `changed=false` with identical bytes/hash.
- [x] 4.6 Run a minimal project tool round-trip through OpenAI, Claude and Ollama scripted provider adapters and verify all three receive the same qualified definitions and produce the same trace-derived result.
- [x] 4.7 Make symbol/call/dependency lookup CodeGraph-first with bounded search fallback, separate discovered paths from actually read paths in the trace summary, and add a scripted ToolExecutor regression that reads relevant source without broad documentation search.
- [x] 4.8 Normalize denylisted-path failures into a safe `policy_denied` trace outcome, render a deterministic refusal without path existence/content disclosure, and add a scripted `.env-mcp` regression that does not produce the generic missing-research error.
- [x] 4.9 Add a real loopback `ProjectMcpRuntime` regression for a denied env-file read and classify the exact typed `rmcp::ServiceError::McpError` message so protocol display prefixes cannot bypass the safe refusal.

## 5. Interactive integration and documentation

- [x] 5.1 Add `/project` to CLI help, completion and Russian diagnostics, start `ProjectMcpRuntime` independently beside help runtime, route only `/project <цель>` to it and preserve ordinary chat and `/help` on startup failure; verify command parsing and unavailable-runtime tests make zero provider calls.
- [x] 5.2 Wire parent-session recording so only the cleaned goal and final visible summary are saved, with no `/project` prefix or provider-specific tool messages; verify session regression tests cover successful read-only, write, cancellation and failure cases.
- [x] 5.3 Create `reports/day34/README.md` with architecture, safety boundaries, the ToolExecutor search scenario, the `reports/day34/commands-audit.md` generation/update scenario, expected diff/no-op behavior and commands to reproduce them; verify the report contains no credentials, env contents, local database data or personal dialogue.

## 6. Final verification

- [x] 6.1 Run `cargo fmt --all -- --check` and fix formatting until it exits successfully.
- [x] 6.2 Run `cargo clippy --all-targets --all-features -- -D warnings` and fix every warning until it exits successfully.
- [x] 6.3 Run `cargo test` and verify all existing and new unit, async, loopback and scripted project scenarios pass without live credentials.
- [x] 6.4 Run `openspec validate add-project-file-assistant --strict` and verify every proposal, design, delta spec and task artifact passes strict validation.
