## 1. Retrieval configuration and baseline

- [x] 1.1 Add validated `RetrievalConfig` defaults (`candidate_k=20`, `final_k=5`, `min_similarity=0.40`, `context_chars=6000`) and verify unit tests reject zero or inverted K values, non-finite/out-of-range thresholds and a zero context budget before provider calls.
- [x] 1.2 Preserve the Day 22 baseline retrieval as an explicit internal path parameterized by `final_k` and verify regression tests retain cosine ordering, `chunk_id` tie-breaking, default top-5 selection, configurable top-`final_k` and whole-chunk context budgeting without rewrite, threshold or reranking.
- [x] 1.3 Extend retrieval result and trace types with the two similarities, semantic/rerank scores, applied settings and safe candidate counts/metadata, and verify serialization used by evaluation excludes content, vectors and hashes from candidate traces.

## 2. Query rewrite

- [x] 2.1 Add a provider-neutral `QueryRewriter` abstraction and live implementation using an isolated `RequestClient` call with the current provider/model, temperature 0 and dedicated rewrite instructions; verify a recording-client test receives exactly one message, does not inherit active mode instructions and receives no history, profile, memory, invariants or tool definitions.
- [x] 2.2 Add the untrusted-data rewrite prompt, protected-element extraction and output normalization/validation with a 512-character limit; verify tests cover Unicode case folding, preserved and missing numbers, digit/`_`/`-` identifiers, quoted/backticked fragments, collapsed whitespace, empty output, oversized output and provider failure.
- [x] 2.3 Wire rewrite usage and duration into the safe retrieval trace and verify failures stop enhanced retrieval before embedding and generation without falling back to baseline.

## 3. Filtering and heuristic reranking

- [x] 3.1 Implement Unicode-aware deterministic tokenization with the fixed Russian/English stop-word set while preserving numbers and `_`/`-` technical identifiers; verify focused unit tests cover Cyrillic, case folding, duplicates and short-token rules.
- [x] 3.2 Implement content and metadata coverage plus `0.70 * semantic + 0.20 * content + 0.10 * metadata`, and verify unit tests demonstrate that exact technical terms can reorder close semantic candidates and ties resolve by `chunk_id`.
- [x] 3.3 Implement enhanced retrieval using one two-query embedding batch, one structural-index scan, maximum similarity, top-`candidate_k`, threshold filtering, reranking and final top-`final_k`, returning a typed `Retrieved` or `NoRelevantContext` outcome; verify tests cover deduplication, ordering, candidate counts, threshold boundaries, the non-error empty filtered outcome, incompatible/zero vectors and context budget behavior.

## 4. Interactive RAG integration

- [x] 4.1 Connect interactive `/rag on` to enhanced retrieval using the current agent settings and default retrieval configuration, and verify tests show the generation request and persisted history still contain the original question rather than the rewritten query.
- [x] 4.2 Update Russian CLI progress/error and source output for rewrite/filter/rerank results without exposing candidate contents, vectors, credentials or the service prompt; verify snapshot/string assertions cover success, the typed no-relevant-context outcome without generation, protected-element rewrite rejection and provider failure.
- [x] 4.3 Keep `/rag off`, `/rag status`, `/new` behavior and automatic task-continuation requests unchanged, and verify the existing RAG command and session regression tests pass.

## 5. Baseline/enhanced evaluation

- [x] 5.1 Extend `rag-eval` parsing with unique `--candidate-k`, `--final-k` and `--min-similarity` options and defaults, and verify invalid, duplicate, missing and unknown arguments fail before network access.
- [x] 5.2 Replace the plain/RAG evaluation pair with independent `baseline_rag`/`enhanced_rag` contexts over the existing Day 22 control questions, using the same `final_k` in both modes; verify recording-client tests observe no plain request, no state leakage, configurable equal result limits and no enhanced generation call for `NoRelevantContext`.
- [x] 5.3 Update the atomic JSON schema with retrieval settings, per-mode outcome status, rewritten query, safe before/after candidate metadata, per-stage usage/durations and separate mode aggregates; verify serialization and aggregation tests record `NoRelevantContext` with empty sources, no answer, zero fact coverage and completed-stage costs, count it separately, include rewrite cost in enhanced totals and exclude secrets, vectors, hashes, full candidate text and the rewrite service prompt.
- [x] 5.4 Update terminal progress for both RAG modes and verify multiline/control-character sanitization and safe source/score reporting remain intact.

## 6. Day 23 experiment and report

- [x] 6.1 Run the two-mode evaluation against `reports/day22/control-questions.json` with the documented defaults and atomically produce `reports/day23/comparison.json`; verify it contains 10 complete baseline/enhanced outcome pairs, retains any `NoRelevantContext` result without aborting and has no plain section.
- [x] 6.2 Create `reports/day23/README.md` with the reproduction command, fixed settings and weights, per-question and aggregate quality/cost/latency comparison, and manual analysis of improvements and regressions; verify every stated number is traceable to `comparison.json` and string metrics are described as heuristics.

## 7. Final verification

- [x] 7.1 Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test`, and record successful results in the Day 23 report.
- [x] 7.2 Run `openspec validate add-rag-rewrite-reranking --strict` and verify the completed change remains consistent with the modified `rag-query` contract.
