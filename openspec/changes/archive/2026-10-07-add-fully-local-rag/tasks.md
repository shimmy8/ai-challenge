## 1. Ollama RAG integration verification

- [x] 1.1 Add or extend a recording-client regression proving that enhanced RAG query rewrite receives `Provider::Ollama`, the selected local model, isolated rewrite instructions, and no MCP tools; verify the focused async test passes without network access.
- [x] 1.2 Add a regression proving that grounded generation and the optional single repair request use the same Ollama settings and never switch to OpenAI or Claude; verify fixtures cover valid first-pass, repaired, and invalid-final responses.
- [x] 1.3 Verify the Ollama RAG path preserves the existing no-context result, source rendering, citation validation, original user message persistence, token metrics, and generation/repair counts; add only missing focused assertions and run the responsible RAG tests.
- [x] 1.4 If the regressions expose an actual provider dispatch or Ollama response incompatibility, implement the smallest provider-neutral fix and verify existing OpenAI and Claude behavior remains unchanged; otherwise record that no production-code change was required.

## 2. Interactive control sessions

- [x] 2.1 Verify `.fox-index.db` is the unchanged Week 6 index with compatible `text-embedding-3-small` model, dimensions, and published structural chunks using existing safe diagnostics; do not rebuild, migrate, copy, or publish the database.
- [x] 2.2 Freeze the previous week's control questions and their exact order, plus the clean-session conditions: no profile, active task, custom response mode, or unrelated history; verify the checklist is recorded before either provider run.
- [ ] 2.3 Start `cargo run -- --dump-metrics`, select Ollama with `qwen3.5:4b`, create a clean session, enable `/rag on`, and ask the full control set in order; verify the session is saved and record only its exact id or title for later analysis.
- [x] 2.4 If variability needs direct evidence, repeat two or three predetermined difficult questions at the end of the Ollama session; verify the repeats are clearly distinguishable from the primary control sequence. Repeats were unnecessary because all six primary turns directly exhibited the same generation/repair contract instability; the run stopped for thermal comfort.
- [ ] 2.5 If an already configured cloud provider is available, create a second clean session and repeat the same primary questions and optional repeats in the same order; otherwise record that optional cloud comparison was unavailable without requesting or exposing credentials.

## 3. Selected-session analysis

- [x] 3.1 After the user explicitly identifies the control session ids or titles, read only those saved sessions and extract provider, model, ordered questions, answers, sources, and citations; verify no unrelated session content is read or copied.
- [x] 3.2 Select only `fox-metrics.log` entries matching the identified session ids and correlate them chronologically with completed turns; verify count/order consistency and mark ambiguous or missing elapsed/token values as unavailable instead of inferring them.
- [x] 3.3 Evaluate each answer against the previous week's expected facts and sources, recording correctness, completeness, citation usefulness, grounded failures, and explicit no-context outcomes; verify every judgment points to a control question and saved answer.
- [x] 3.4 Compute per-provider success rate, elapsed-time range and median, token totals when available, provider/contract error counts, missing-source counts, and repeated-question variability; verify aggregates derive only from selected sessions and matching metrics entries.

## 4. Day 28 report

- [x] 4.1 Create `reports/day28/README.md` describing the reused Week 6 index, request flow, environment, exact interactive commands, selected models, control-question order, session ids, and analysis method; verify no step instructs the user to reindex or change embeddings.
- [x] 4.2 Add question-by-question quality results and aggregate speed/stability tables for Ollama and the optional cloud session; verify each number and conclusion is traceable to a selected session or matching metrics entry. The report explicitly marks questions 7–10 and cloud comparison as not run.
- [x] 4.3 State the locality boundary explicitly: SQLite vector search and Ollama rewrite/generation are local, while query embeddings still use the existing configured endpoint; verify the report does not claim completely offline execution when that endpoint is external.
- [x] 4.4 Add a concise recording checklist showing the existing index, Ollama selection, `/rag on`, representative verified sources, and comparison summary; verify it excludes API keys, local configuration contents, databases, full metrics logs, vectors, full retrieval chunks, unrelated dialogues, and secret env files.

## 5. Quality gates

- [x] 5.1 Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`; fix failures and record final command outcomes and test counts in the Day 28 report.
- [x] 5.2 Run `openspec validate add-fully-local-rag --strict` and verify the proposal, specs, design, and tasks consistently require the existing Week 6 index and interactive session analysis rather than `rag-eval`.
