## 1. Scheduler Domain and Storage

- [x] 1.1 Add scheduler job, schedule, run, status, trigger and aggregate result types in a responsibility-focused module; verify unit tests cover valid `once`/`daily` parsing, the `Europe/Moscow` default, invalid combinations and UTC `next_run_at` calculation.
- [x] 1.2 Add `.fox-scheduler.db` path handling, Git ignores and an idempotent `SchedulerStore` schema for jobs and runs with Unix `0600` permissions; verify a tempfile SQLite test opens the store twice and inspects all expected columns and constraints.
- [x] 1.3 Implement create/list/load, pause/resume, soft-delete and bounded history aggregation without storing credentials; verify CRUD tests cover state transitions, missing IDs, preserved run history and absence of secret-shaped fields.
- [x] 1.4 Implement transactional due-job claim, manual claim and `next_run_at` advancement; verify controlled-clock SQLite tests prove one claim per occurrence, no overlap, manual runs preserve the planned time, daily catch-up occurs at most once and one-time jobs do not repeat.
- [x] 1.5 Implement startup recovery that marks unfinished runs interrupted and exposes aggregate counters; verify a reopen test simulates process termination and observes an interrupted run plus the correct next eligible execution.

## 2. Calendar Collection

- [x] 2.1 Extend the CalDAV client with validated absolute range reads using `REPORT calendar-query` and the existing unambiguous calendar selection; verify request fixture tests cover UTC time-range construction, invalid bounds, one calendar and ambiguous calendars.
- [x] 2.2 Add safe WebDAV/iCalendar response normalization for timed and all-day VEVENTs, unfolded lines, optional location, ordering and per-event warnings; verify fixtures cover empty results, multiple ordered events, a malformed event beside valid events and a fatal malformed server response.
- [x] 2.3 Harden calendar read errors and diagnostics so credentials and raw private responses cannot escape; verify regression tests inject recognizable secrets into settings/responses and assert that returned errors and captured log-safe diagnostics omit them.

## 3. Digest Generation and Telegram Delivery

- [x] 3.1 Implement deterministic Russian calendar summaries for empty and non-empty intervals with bounded event input; verify exact-output tests cover timed events, all-day events, optional locations and an empty calendar.
- [x] 3.2 Implement an isolated `DigestGenerator` that reuses the provider request path with fixed instructions, no conversation memory and empty tool options; verify fake-provider tests inspect the complete request and prove that provider failure, empty text or a tool call selects the deterministic fallback.
- [x] 3.3 Extend the MCP dotenv loader with an exact allowlist for `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID`, and add safe Telegram settings validation; verify parser tests accept documented variables, reject unrelated names and never include configured values in errors.
- [x] 3.4 Implement a Telegram `sendMessage` adapter with `delivered`, `failed` and `unknown` outcomes; verify local HTTP fixture tests cover `ok=true`, explicit API rejection and transport failure without leaking token, chat ID or raw response bodies.
- [x] 3.5 Implement the run pipeline `collecting -> generating -> delivering -> terminal status` and persist the actual text source and outcome at every boundary; verify fake-component tests prove CalDAV failure skips LLM and Telegram, LLM failure still delivers fallback, and uncertain Telegram delivery is not retried automatically.

## 4. Background Runtime

- [x] 4.1 Implement `SchedulerRuntime` with injected clock, calendar reader, generator and notifier plus wake/cancellation signals; verify paused-time async tests execute a due job without an MCP client and shut the worker down cleanly.
- [x] 4.2 Start exactly one runtime before constructing MCP sessions and share its handle with every server instance; verify an integration test opens multiple MCP sessions against one ephemeral server and observes a single run for one due occurrence.
- [x] 4.3 Load the configured provider/model snapshot from `.fox-llm.json` for server mode without loading session history or memory; verify configuration tests cover explicit provider, `last_provider`, missing provider/key fallback and storage of provider/model without API key.

## 5. MCP Control Plane

- [x] 5.1 Register schemas and handlers for creating and listing calendar digest schedules with validated `once`/`daily` inputs, horizon limits and safe results; verify MCP discovery and call tests assert annotations, required fields, defaults and persisted `next_run_at`.
- [x] 5.2 Add pause, resume, soft-delete and manual-run handlers, marking every mutation non-read-only; verify executor tests require the existing confirmation path and prove rejected calls leave scheduler state unchanged.
- [x] 5.3 Add the read-only history tool with bounded recent runs, aggregate status counts and latest summary; verify tool tests assert read-only discovery, stable JSON shape and absence of Telegram, CalDAV and provider credentials.
- [x] 5.4 Preserve existing `echo` and `create_calendar_event` behavior while sharing the renamed server state; verify the existing MCP handshake, tool allowlist, calendar creation and provider-neutral tool execution regression tests still pass.

## 6. Documentation and Operational Setup

- [x] 6.1 Update `.env-mcp.example` with placeholder-only Telegram settings and document bot/chat setup without real credentials; verify the example contains no token-like value and the secret files remain ignored by Git.
- [x] 6.2 Add `reports/day18/README.md` describing architecture, MCP tool examples, manual-run verification, SQLite aggregation and the boundary that delivery requires a continuously running MCP process; verify every command and tool name matches the implemented CLI discovery output.
- [x] 6.3 Update the root README with scheduler startup, Telegram configuration, allowlist enablement and recovery semantics; verify a clean setup path explains that provider configuration changes require an MCP-server restart and that `unknown` delivery is not automatically retried.

## 7. Final Verification

- [x] 7.1 Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test`; record the commands and successful results in the Day 18 report.
- [x] 7.2 Perform an opt-in manual smoke test with local credentials: start `--mcp-server`, call `run_calendar_digest_now`, create a near-future one-time schedule and inspect `get_calendar_digest_history`; record only redacted outcomes and never commit `.env-mcp`, scheduler databases, Telegram content or API credentials.
