## 1. Provider-neutral tool calling

- [x] 1.1 Add provider-neutral tool definition, tool call, tool result, request-options, and model-turn types; adapt `RequestClient` so ordinary text-only calls remain backward-compatible, and verify existing agent tests plus new no-tools tests pass.
- [x] 1.2 Extend the OpenAI Responses payload builder and response parser for native function definitions, function calls, call identifiers, and function-call outputs; verify focused JSON fixture tests cover a direct final answer and a tool-call continuation.
- [x] 1.3 Extend the Claude Messages payload builder and response parser for `tools`, `tool_use`, `tool_result`, and stop reasons; verify equivalent focused fixture tests pass for a direct final answer and a tool-call continuation.
- [x] 1.4 Ensure summary, compression, task-support, and invariant-verifier requests always use empty tool options; verify regression tests exercise every auxiliary request path with MCP tools enabled.

## 2. MCP execution and agent orchestration

- [x] 2.1 Preserve each MCP tool's input schema and read-only/destructive annotations from `tools/list`, filter the full definitions through the saved allowlist, and verify focused tests reject disabled, stale, and duplicate names.
- [x] 2.2 Implement a request-scoped MCP session that performs handshake, one current `tools/list`, time-bounded `tools/call`, safe error mapping, and clean shutdown; verify asynchronous tests use an ephemeral local server for success, timeout, disappearance, and protocol failure.
- [x] 2.3 Add injectable tool-executor and approval interfaces and implement the agent loop `model turn -> validation/approval -> MCP result -> model turn` with a maximum of three calls; verify scripted tests cover success, plain-text non-invocation, user cancellation, changed arguments requiring reapproval, and rejection of a fourth call.
- [x] 2.4 Keep provider-specific tool traces request-scoped while persisting only the user input and checked final answer; verify session, sliding-window, sticky-facts, summary, and branching regression tests preserve their current history behavior.
- [x] 2.5 Run invariant preflight against the canonical proposed action before confirmation, retain final draft verification, and surface a deterministic successful-action notice if synthesis fails after a write; verify tests prove violating actions are not offered or executed and post-write failures are not reported as rollback.

## 3. Yandex CalDAV MCP tool

- [x] 3.1 Add the narrowly required XML, date/time, UUID, and iCalendar support while reusing the existing HTTP client where practical; verify `cargo build` succeeds without enabling unrelated dependency features.
- [x] 3.2 Define and register `create_calendar_event` with its description, state-changing annotation, required/optional JSON Schema fields, strict RFC 3339 parsing, and `end > start` validation; verify MCP integration tests inspect discovery metadata and reject malformed calls before network access.
- [x] 3.3 Load the CalDAV username, app password, optional calendar display name, and test-overridable endpoint from environment-backed server settings without exposing their values; verify tests cover missing/rejected credentials and assert secrets are absent from errors, results, metrics-ready values, and debug output.
- [x] 3.4 Implement CalDAV principal/home/collection discovery and writable-calendar selection: auto-select exactly one, match an explicitly configured name, and refuse ambiguous selection; verify a local mock covers one calendar, multiple calendars, missing name, unknown name, and read-only collections.
- [x] 3.5 Generate an escaped RFC-compatible iCalendar payload with one stable UUID, create it by CalDAV PUT without duplicate creation, and map definite failure versus uncertain transport outcome; verify mock-server tests inspect URL, headers, UTC timestamps, optional fields, CRLF body, idempotent retry behavior, and safe returned event summary.
- [ ] 3.6 Exercise `create_calendar_event` through the real local MCP Streamable HTTP transport backed by mock CalDAV and verify exactly one confirmed event is written and its safe result is returned to the client.

## 4. CLI confirmation and temporal context

- [x] 4.1 Build the MCP runtime from the existing optional server URL and allowlist and inject it into the single interactive agent without changing `.fox-llm.json`; verify no-server and empty-allowlist startup paths make ordinary requests without an MCP connection.
- [x] 4.2 Implement Russian CLI previews and `да/нет` confirmation from canonical calendar arguments, return `cancelled_by_user` without calling MCP on rejection, and print a separate completed-action notice when necessary; verify approval tests compare the displayed fields with the exact executed arguments.
- [x] 4.3 Add the current absolute date/time and `Europe/Moscow` to main-request instructions and require clarification when start time or duration is absent; verify deterministic-clock tests cover «завтра в 15:00 на час», missing time, missing duration, and a day-boundary case.
- [x] 4.4 Verify both OpenAI and Claude scripted end-to-end paths perform discovery, request the same confirmed calendar action, consume the MCP result, and produce a final answer while token/session metrics include every provider turn.

## 5. Documentation and acceptance

- [x] 5.1 Update `README.md` with the Day 17 tool-call flow, Yandex app-password setup, `YANDEX_CALDAV_USERNAME`, `YANDEX_CALDAV_PASSWORD`, optional `YANDEX_CALDAV_CALENDAR`, security warnings, and the confirmation/clarification behavior; verify every documented command and variable matches the implementation.
- [x] 5.2 Create `reports/day17/README.md` with architecture, registered input schema, expected tool result, test evidence, exact two-terminal demonstration steps, expected confirmation text, and a concise video shot list; verify the scenario starts from a disabled tool and ends with visual confirmation in Яндекс Календаре.
- [x] 5.3 With user-provided local environment variables, run the manual scenario «Запланируй встречу с Анной завтра в 15:00 на час», approve the preview, confirm exactly one event appears with matching fields, then remove the test event manually and record only non-secret observations in the Day 17 report.
- [ ] 5.4 Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, and `openspec validate add-yandex-calendar-mcp-tool --strict`; record the successful commands in `reports/day17/README.md` without including credentials or personal calendar data.
