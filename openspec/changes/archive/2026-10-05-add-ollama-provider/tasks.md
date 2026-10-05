## 1. Provider identity and configuration

- [x] 1.1 Add `Provider::Ollama`, display/serialization metadata, API-key requirement metadata, and session provider mapping; verify focused tests round-trip `ollama` through config and SQLite session metadata.
- [x] 1.2 Add Ollama defaults (`qwen3.5:4b`, no key) and normalize both current and legacy configs that lack the provider; verify migration tests preserve existing providers, temperatures, MCP settings, profiles, tasks, and memory data.
- [x] 1.3 Make `AgentSettings` credentials optional while retaining mandatory non-empty keys for OpenAI and Claude; verify unit tests accept keyless Ollama and continue to reject missing cloud credentials.

## 2. Ollama Responses transport

- [x] 2.1 Refactor the existing OpenAI Responses request construction and response parsing into a reusable Responses-compatible path without changing OpenAI behavior; verify current OpenAI text, usage, tool-call, and continuation tests still pass.
- [x] 2.2 Add Ollama dispatch to `http://127.0.0.1:11434/v1/responses` without an authorization header and preserve instructions, history, temperature, tools, tool calls, and tool results; verify transport-spec tests cover the URL, missing auth, payload, text response, usage, and structured tool calls.
- [x] 2.3 Add bounded Russian diagnostics for connection refusal, missing configured model, unsupported Responses API, and other HTTP failures with no cloud fallback; verify focused error-classification tests assert each observable message and secret redaction.

## 3. CLI and local model discovery

- [x] 3.1 Fetch installed Ollama models from `http://127.0.0.1:11434/v1/models`, accept arbitrary non-empty local model names, and retain OpenAI/Claude filtering rules; verify parser tests cover sorting, deduplication, non-OpenAI names, malformed responses, and an empty list.
- [x] 3.2 Include Ollama in provider selection, skip the authorization prompt, use the normal model/reconfigure commands, and keep status/temperature output valid; verify CLI helper tests and a scripted interaction demonstrate switching between a cloud provider and Ollama without modifying saved cloud keys.
- [x] 3.3 Return actionable guidance when Ollama has no installed models or is not running during discovery; verify focused tests distinguish these cases and leave the selected model unchanged.

## 4. Provider-neutral agent behavior

- [x] 4.1 Extend provider matrices so Ollama receives enabled MCP definitions, can return a structured tool call, receives the actual tool result, and produces the checked final answer; verify the existing provider-neutral MCP integration tests pass for all three providers.
- [x] 4.2 Verify Ollama flows through memory/profile prompts, response modes, context compression, RAG query rewrite, metrics, and isolated `ai.generate_text` without cloud credentials or leaked interactive context; add targeted regressions for every behavior not already covered by a provider-neutral test.
- [x] 4.3 Verify saved Ollama conversations can be reopened and that `/new`, `/remember`, `/forget`, tasks, and profiles retain their existing isolation guarantees; run the relevant temporary-SQLite regression tests.

## 5. Day 26 demonstration and quality gates

- [x] 5.1 With Ollama 0.13.3+ and `qwen3.5:4b` installed, run one simple, one planning, and one Rust coding request through `fox-llm`, plus one direct HTTP request, and record reproducible commands and observed results in `reports/day26/`.
- [x] 5.2 Add a concise recording checklist in `reports/day26/` showing local process/model evidence, CLI provider selection, three responses of increasing complexity, and the relevant code diff; verify the checklist contains no API keys, personal dialogues, local database contents, or secret env values.
- [x] 5.3 Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`; fix failures and record the successful commands in the Day 26 report.
