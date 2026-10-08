## 1. Benchmark interface and data model

- [x] 1.1 Add the `summarize-eval` startup mode with explicit dataset, profiles and output arguments, and verify parser tests accept one valid invocation and reject missing, duplicate, conflicting and unknown arguments.
- [x] 1.2 Create `src/summarization/` with provider-neutral dataset, profile, expected-summary and result types, export it through `src/summarization/mod.rs`, keep `main.rs` thin, and verify the project builds without changing existing startup modes.
- [x] 1.3 Implement complete preflight validation for unique IDs, `short`/`medium`/`long` coverage, non-empty synthetic transcripts, profile bounds and well-formed expectations, and verify invalid fixtures fail before a mock request client observes any generation call.

## 2. Ollama profiles and runtime preflight

- [x] 2.1 Add versioned baseline, compact and balanced profile configuration plus three `reports/day29/profiles/Modelfile.*` files that all derive from `qwen3.5:4b`, and verify parsed values match the documented temperature, `num_ctx`, `num_predict`, prompt kind and expected `Q4_K_M` quantization.
- [x] 2.2 Implement Ollama `/api/show` preflight for alias availability, family and quantization without reading secret env files, and verify mocked missing, incompatible and valid model responses produce the required Russian outcomes without cloud fallback.
- [x] 2.3 Implement `/api/ps` runtime metadata collection for exact alias, context length, model size, loaded memory and quantization, and verify absent optional fields become `null` with an explicit reason rather than zero.

## 3. Isolated summary generation

- [x] 3.1 Implement the general and specialized meeting-summary prompts with the strict single-object JSON contract, later-decision precedence, nullable missing values and five-sentence summary limit, and verify snapshot-style tests cover all required instructions without including the application's unrelated memory, RAG or MCP context.
- [x] 3.2 Execute each scenario as an independent single-turn Ollama request using the selected alias and temperature, rotate profile order by scenario, mark the first request per alias as cold and later requests as warm, and verify mock call traces contain no history, tool calls or state from previous runs.
- [x] 3.3 Parse exactly one JSON object into the summary schema while preserving sanitized diagnostics for invalid JSON, Markdown fences or surrounding prose, and verify one malformed response does not prevent later independent scenarios from running.
- [x] 3.4 Add stderr progress logs for start, profile preflight, each scenario/profile start and completion, cold/warm state, aggregate and report publication, and verify capture tests expose only allowlisted IDs and numeric metrics without transcript, raw answer, prompt or authorization data.

## 4. Quality and performance evaluation

- [x] 4.1 Implement deterministic checks for decisions, action items, owners, deadlines, open questions, risks, superseded facts, unsupported facts and summary length against normalized benchmark annotations, and verify focused tests distinguish full success, missing facts, stale decisions and hallucinations.
- [x] 4.2 Compute per-case elapsed time, available token counts and output tokens per second, and verify throughput is `null` with a reason when output tokens or timing are unavailable instead of emitting an invalid numeric value.
- [x] 4.3 Aggregate pass counts and performance statistics by profile and length tier, keeping cold and warm observations distinguishable, and verify a long-context regression cannot be hidden by short-case successes in the overall aggregate.
- [x] 4.4 Select the recommended optimized profile by quality first and median warm latency and loaded memory only as tie-breakers, and verify equal-quality and unequal-quality fixtures produce deterministic recommendations rather than assuming `balanced` wins.

## 5. Fixtures and safe result artifacts

- [x] 5.1 Add nine synthetic Russian meeting protocols—three short, three medium and three long—with gold annotations covering cancellations, changed owners, shifted deadlines, open questions and missing nullable values, and verify every fixture passes dataset preflight without personal or credential-like data.
- [x] 5.2 Implement atomic sanitized JSON output with per-case diagnostics, profile/tier aggregates and runtime metadata, and verify interruption or serialization failure preserves an existing successful report and leaves no partial result presented as final.
- [x] 5.3 Add redaction/allowlist tests proving results omit authorization headers, API keys, env contents, full application system prompts and unrelated local state while retaining the benchmark prompt identifier and reproducibility parameters.

## 6. Experiment and documentation

- [x] 6.1 Document alias creation, benchmark execution and optional alias cleanup in `reports/day29/README.md`, and verify every command uses only `.env-mcp.example`-safe public values and the existing local `qwen3.5:4b` weights.
- [x] 6.2 Run the complete benchmark against local Ollama, preserve the sanitized machine-readable result in `reports/day29/`, and verify all 27 planned profile/scenario combinations are present or carry an explicit diagnostic outcome.
- [x] 6.3 Complete the Day 29 report with before/after quality, latency, tokens, loaded-memory observations, limitations and the selected profile, and verify every conclusion is traceable to the saved result without claiming unavailable temperature or whole-system energy measurements.

## 7. Regression verification

- [x] 7.1 Add regression tests in `src/tests.rs` for startup parsing, dataset validation, profile isolation, strict JSON parsing, scoring, aggregation, runtime metadata and atomic output, and verify `cargo test` passes.
- [x] 7.2 Run `cargo fmt --all -- --check` and `cargo clippy --all-targets --all-features -- -D warnings`, resolving all new formatting and lint failures without modifying unrelated user changes.
- [x] 7.3 Run `openspec validate optimize-local-meeting-summarization --strict` and verify the completed implementation still satisfies every scenario in `local-meeting-summarization`.
