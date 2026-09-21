## 1. MCP configuration and startup modes

- [x] 1.1 Add the official `rmcp` dependency and only the Streamable HTTP client/server features required by the design; run `cargo build` to verify the selected feature set compiles with the existing dependency graph.
- [x] 1.2 Add a serde-defaulted single-server MCP configuration with optional URL and unique enabled-tool names to `.fox-llm.json`; add round-trip and pre-MCP/legacy loading tests and verify them with `cargo test config`.
- [x] 1.3 Replace the single-purpose argument check with an explicit interactive, metrics, or MCP-server startup mode while preserving `cargo run` and `--dump-metrics`; add parser tests for valid and conflicting arguments and verify the focused tests pass.

## 2. MCP protocol and demonstration server

- [x] 2.1 Implement an MCP client module that validates absolute HTTP(S) endpoint URLs, performs a time-bounded handshake and `tools/list`, returns display-ready tool descriptors, and closes the session; verify URL, timeout, and error-mapping behavior with focused asynchronous tests.
- [x] 2.2 Implement the loopback-only `--mcp-server` mode at the documented `/mcp` endpoint with the `echo(text)` tool, keeping startup/protocol output separated; verify the server starts through `cargo run -- --mcp-server` and rejects an occupied bind address with a clear error.
- [x] 2.3 Add an isolated integration test using an automatically allocated loopback port; verify handshake succeeds, `tools/list` returns the named and described `echo` schema, and calling `echo` returns the original text.

## 3. Interactive `/mcp` workflow

- [x] 3.1 Register `/mcp` in command completion, `/help`, and the application dispatch loop; verify command inventory tests and an ordinary non-MCP startup remain unchanged.
- [x] 3.2 Implement the single-server status/setup menu with retry, change-address, and cancel paths, committing a new URL only after successful handshake plus `tools/list`; verify pure state-transition tests preserve the previous configuration on invalid URLs, connection failures, and cancellation.
- [x] 3.3 Implement tool display and `MultiSelect` enable/disable flow with descriptions, new tools disabled by default, commit-on-confirm, and removal of stale names on confirmed save; verify focused tests cover selection, cancellation, deduplication, and a disappeared tool.
- [x] 3.4 Exercise the interactive client manually against the local server and verify `/mcp` shows the active address, lists `echo`, persists its enabled state across restart, and offers a new address after the server is stopped.

## 4. Documentation and deliverable

- [x] 4.1 Update `README.md` with `/mcp`, the `.fox-llm.json` MCP section, the `--mcp-server` command, default endpoint, failure behavior, and the explicit Day 16 boundary that enabled tools are not yet invoked by the LLM; verify every documented command matches the implemented CLI.
- [x] 4.2 Add `reports/day16/README.md` containing the task result, architecture summary, exact two-terminal demonstration sequence, expected output, and a concise shot list for the required video; perform the sequence once and verify the recorded steps are reproducible without API calls to external LLM providers.

## 5. Final verification

- [x] 5.1 Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`; fix any failures and record the successful commands in the Day 16 report.
