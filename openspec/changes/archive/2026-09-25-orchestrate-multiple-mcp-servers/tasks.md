## 1. Multi-server configuration and CLI

- [x] 1.1 Replace the singleton MCP config with validated `McpServerConfig { id, url, disabled_tools }` entries, intentionally reject the old shape, and verify round-trip, duplicate-ID, invalid-ID and fresh-config tests pass.
- [x] 1.2 Rework `/mcp` into a registry menu for add, inspect, URL replacement, removal and denylist editing, and verify cancellation and a failure of one server leave all unrelated entries unchanged.
- [x] 1.3 Extend startup parsing to accept `--mcp-server <kind> --addr <loopback-address>`, reject non-loopback binds and unknown kinds, and verify focused CLI parsing tests pass.

## 2. Qualified routing runtime

- [x] 2.1 Add server IDs, qualified tool names and explicit tool outcomes to provider-neutral MCP types, and verify serialization/validation tests cover invalid separators, name collisions and `success`/`failed`/`unknown` outcomes.
- [x] 2.2 Implement `McpRouter` with independent sessions, route-table-backed qualified definitions and native-name translation, and verify two servers exposing the same native name route to the correct endpoint.
- [x] 2.3 Build the router in degraded mode from all configured servers, preserve successfully connected servers when peers fail, and verify disabled, stale and unavailable routes fail closed.
- [x] 2.4 Switch the agent pool and both providers to qualified multi-server definitions while keeping tools out of auxiliary requests, and verify OpenAI and Claude request tests receive the same enabled catalog.

## 3. Server responsibility split

- [x] 3.1 Split the monolithic handler into `github`, `reporting`, `workspace` and `calendar` handlers under `src/mcp/`, and verify each `tools/list` contains only its assigned tools and annotations.
- [x] 3.2 Rename public native tools to their concise server-scoped names, remove `echo` and its request/tests, and verify no local server advertises or executes `echo`.
- [x] 3.3 Keep workspace writes confined to the configured report root and verify new-file, overwrite, traversal and symlink regression tests through `workspace__save_report`.
- [x] 3.4 Expose Calendar `list_events` with absolute and relative interval inputs, and verify timezone, tomorrow-boundary, empty-calendar, ambiguous-calendar and credential-redaction tests.

## 4. Shared pipeline execution

- [x] 4.1 Move parsing, full-plan validation, reference resolution, sequential execution and trace creation into `src/mcp/pipeline.rs`, and verify existing success, timeout, failure and skipped-step behavior remains covered.
- [x] 4.2 Extend references to nested output fields and `run.id`, `run.trigger`, `run.scheduled_at` while preserving JSON types, and verify unknown, forward and schema-invalid references fail before the target call.
- [x] 4.3 Implement interactive and scheduled execution policies, including invariant callbacks, per-write interactive approval and immutable-plan scheduled authorization, and verify each mode applies only its intended confirmation policy.
- [x] 4.4 Add server ID, native tool, ordinal, duration and explicit outcome to step traces, and verify a failed step after a successful write reports completed effects without rollback claims.

## 5. AI and Telegram servers

- [x] 5.1 Implement the isolated `ai` handler with `generate_text`, bounded inputs and a provider request containing no history, memory or tools, and verify empty results, returned tool calls and provider failures are rejected safely.
- [x] 5.2 Implement the `telegram` handler with fixed local destination and `send_message`, and verify delivered, rejected, unknown and missing-config outcomes without exposing token or chat ID.
- [x] 5.3 Ensure AI and Telegram load only their own required configuration and that neither server accepts credentials or destination fields in tool arguments; verify unknown-field schema tests and redaction assertions pass.

## 6. Generic scheduled jobs

- [x] 6.1 Replace the calendar-specific scheduler schema with generic jobs, runs and run-step traces containing schedule JSON, pipeline JSON and a canonical authorization hash, and verify persistence, restart recovery and interrupted-run tests on a temporary SQLite database.
- [x] 6.2 Implement generic orchestrator lifecycle tools for create, list, pause, resume, delete, run-now and history, and verify read/write annotations, one-time schedules, daily timezone schedules and manual-run preservation of `next_run_at`.
- [x] 6.3 Validate jobs against a fresh downstream router, prohibit `orchestrator__*` and nested pipeline steps, and verify disabled, missing and schema-changed tools prevent execution without fallback routing.
- [x] 6.4 Execute claimed jobs through `PipelineExecutor` with scheduled runtime context and bounded optional output capture, and verify atomic claiming, at-most-one local run, fail-closed ordering and output-size limits.
- [x] 6.5 Remove `DigestRunner` and calendar-specific scheduler fields/tools after expressing Calendar → AI → Telegram as a generic job, and verify the digest flow records success, source failure, AI failure and unknown delivery correctly.

## 7. Observability and end-to-end validation

- [x] 7.1 Add an injectable metadata-only call logger with silent client routing and server-side start/finish events, and verify correlation IDs, server/tool/step order, statuses and durations are present while arguments, results and known secret markers are absent.
- [x] 7.2 Add a three-server GitHub → Reporting → Workspace integration test that asserts selected endpoints, exact call order, reference propagation, final file output and interactive approval only for the workspace step.
- [x] 7.3 Add a scheduled Calendar → AI → Telegram integration test that asserts runtime date propagation, pre-authorized sink execution, persisted ordered history and no invocation of later steps after failure.
- [x] 7.4 Document commands and example config for launching and registering all named loopback servers, and manually verify stdout shows the complete correlated demonstration flow.
- [x] 7.5 Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test`, and record that all repository checks pass.
