## 1. Provider-neutral pipeline model

- [x] 1.1 Add the `jsonschema` dependency and provider-neutral plan, step, status and trace types in `src/mcp/tools.rs`; verify serialization round-trip and the generated `fox_execute_pipeline` input schema with focused unit tests.
- [x] 1.2 Implement static plan validation for step count, unique ids, enabled tool names, reserved-name conflicts, object arguments and backward-only `$ref` values; verify every rejection occurs before the recording executor receives a call.
- [x] 1.3 Implement recursive whole-output `$ref` resolution and per-step JSON Schema validation; verify nested object/array substitution preserves exact JSON values and invalid resolved arguments fail before execution.

## 2. Agent orchestration and safety

- [x] 2.1 Advertise `fox_execute_pipeline` alongside non-empty enabled MCP definitions for both OpenAI and Claude main requests, while keeping it out of auxiliary requests; verify both provider payload tests and the empty-tool case.
- [x] 2.2 Extend the agent loop to count a pipeline as one top-level call, execute up to eight resolved steps sequentially through `SharedToolExecutor`, and return one structured trace to the provider; verify exact call order and data transfer with a scripted client and recording executor.
- [x] 2.3 Apply invariant verification and approval to each resolved step, stop fail-closed on failure/cancellation, mark remaining steps skipped and preserve completed-write reporting; verify rejection, middle-step failure and failure-after-write regressions.
- [x] 2.4 Keep plans, step calls, results and traces out of saved dialogue history while allowing final synthesis to consume the trace; verify persisted and in-memory user-facing histories contain only the original user message and final assistant answer.
- [x] 2.5 Preserve direct single-tool behavior and the limit of three top-level calls while adding the separate eight-step pipeline limit; verify existing direct-call tests and new boundary tests for 3/4 top-level calls and 8/9 steps.

## 3. GitHub data tools

- [x] 3.1 Add `src/mcp/github.rs` with an injectable-base-URL GitHub client, bounded request budget, required headers, safe error mapping and normalized DTOs; verify mocked responses never expose headers, bodies or local environment details in errors.
- [x] 3.2 Implement `github_repository_metadata` for public repository metadata and languages; verify normalization, invalid `owner/repository`, not-found and rate-limit behavior against a local mock HTTP server.
- [x] 3.3 Implement `github_project_activity` for the bounded period, pagination, contributor/commit statistics, issues, pull requests, Actions and releases, including limited `202` handling and truncation warnings; verify periods 1/365 succeed, 0/366 fail before I/O, and pagination/202 fixtures produce the expected result.
- [x] 3.4 Implement deterministic `calculate_github_metrics` with explicit unavailable values and warnings; verify commit, contributor, closure, merge-time, CI and release calculations with complete, partial and truncated fixtures.

## 4. Report tools and MCP registration

- [x] 4.1 Add `src/mcp/report.rs` and implement `render_github_report` with Overview, Development, Collaboration, Delivery and Warnings sections; verify a golden Markdown fixture distinguishes source, derived and unavailable values.
- [x] 4.2 Implement domain-neutral `save_report_to_file` with safe leaf filename validation, arbitrary UTF-8 content, current-working-directory and symlink checks, default overwrite protection and structured path/byte result; verify new-file save, non-Markdown content, rejected overwrite, confirmed `overwrite: true`, absolute path, separators, `.`/`..` and symlink targets using `tempfile`.
- [x] 4.3 Register all five atomic tools in the local MCP server with accurate schemas and read-only/destructive annotations, and keep all new server logic under `src/mcp/`; verify `tools/list` metadata and direct `tools/call` behavior.
- [x] 4.4 Adjust MCP execution timeouts for bounded GitHub requests without weakening fail-closed behavior; verify timeout tests cover a stalled step and confirm later pipeline steps are not called.

## 5. End-to-end demonstration and documentation

- [x] 5.1 Add an end-to-end scripted pipeline test for `metadata -> activity -> metrics -> render -> save_report_to_file` that verifies each real MCP call receives the previous structured output and produces the expected file in the MCP server working directory only after approval.
- [x] 5.2 Add `reports/day19/README.md` documenting architecture, supported GitHub metrics, setup through `/mcp`, an example user prompt, expected trace and known unauthenticated API limitations; verify all commands and tool names match the implementation.
- [x] 5.3 Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test`; fix all failures and record the successful commands in the Day 19 report.

## 6. Manual-run hardening

- [x] 6.1 Handle persistent GitHub statistics `202 Accepted` with bounded `1s -> 2s -> 4s` backoff and a partial activity result: preserve available sources, mark commits or active contributors unavailable, add a warning, and verify the report pipeline continues with focused mock-server regressions.
- [x] 6.2 Apply a shared 30-second timeout to `github_project_activity` at both the agent step and MCP transport boundaries while preserving the 15-second default for other tools; verify timeout selection and the full regression suite.
- [x] 6.3 Increase the shared `github_project_activity` timeout to 120 seconds for large repositories while preserving the 15-second default for every other tool; update documentation and timeout-selection regression coverage.
- [x] 6.4 Add optional `GITHUB_TOKEN` authentication through the existing `.env-mcp` loader, send it as a sensitive Bearer header, document setup, and verify authenticated requests without exposing the token in errors.
- [x] 6.5 Enrich GitHub activity/reporting: paginate to the requested time boundary within a 20-page safety cap, fall back to distinct commit authors for pending contributor statistics, track incomplete sources separately, pass activity into the renderer, and show raw counts, metadata, language shares and per-source coverage with regression and end-to-end tests.
