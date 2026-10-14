## 1. Минимальный reviewer crate

- [x] 1.1 Зафиксировать characterization-тестами текущие CLI arguments, JSON/Markdown rendering, diff selection и trusted-base review flow до переноса; проверить, что соответствующие review-тесты проходят на исходной структуре.
- [x] 1.2 Создать workspace crate `crates/fox-review` с библиотекой и binary, перенести в него review-модули и добавить узкий review-specific OpenAI adapter; проверить `cargo test --package fox-review` и отсутствие зависимостей от agent, memory, MCP, sessions и interactive CLI в его исходном module graph.
- [x] 1.3 Перевести корневой `fox-llm review-pr` на делегирование библиотеке `fox-review`, сохранив прежние arguments и diagnostics; проверить регрессионными CLI-тестами оба entry point и `cargo test --package fox-llm`.
- [x] 1.4 Сократить manifest нового crate до необходимых dependencies и проверить через `cargo tree --package fox-review --edges normal`, что в его build graph отсутствуют `axum`, `dialoguer`, `rmcp`, `rustyline` и `webbrowser`.

## 2. Устойчивый retrieval

- [x] 2.1 Реализовать content-addressed SQLite cache с metadata для schema/provider endpoint origin/model/dimensions/chunking version и vectors без исходного текста; проверить tempfile-тестами cache hit, miss, несовместимый namespace, SQLite corruption, неверную размерность/non-finite vector и отсутствие credentials/content в базе.
- [x] 2.2 Подключить incremental indexing: подтвердить dimensions первым live query vector, обнаруживать текущий trusted corpus, загружать vectors по content hash, отправлять provider только misses и удалять перед сохранением hashes вне текущего корпуса; проверить fake-provider тестами повторный запуск без corpus embedding calls, частичный reuse после изменения файла, несовпадение runtime dimensions и pruning удалённых chunks.
- [x] 2.3 Добавить типизированную классификацию embedding errors и mock-HTTP тесты для `429`, `5xx`, timeout/transport, `400`/`401`/`403` и невалидного успешного ответа; проверить, что diagnostics не содержат API key и response body.
- [x] 2.4 Реализовать не более трёх embedding-попыток с ограниченным `Retry-After` либо exponential backoff с jitter внутри общего timeout; проверить тестами число вызовов, ограничение задержки, успешный повтор и отсутствие retry для permanent errors.
- [x] 2.5 Реализовать детерминированный lexical retriever по source, section и content с устойчивой сортировкой и существующим Unicode-safe бюджетом; проверить unit-тестами приоритет path/symbol matches, отсутствие zero-score chunks, стабильный tie-break и сохранение целых chunks.
- [x] 2.6 Добавить run-level circuit-breaker: после исчерпания retryable embedding error прекратить последующие embedding calls и использовать lexical retrieval для каждого batch, сохранив только уже проверенные cache rows; проверить fake-provider end-to-end тестами fallback при build и query, отсутствие каскадных вызовов, сохранение diff/test report и фатальность permanent errors.

## 3. Прозрачный degraded результат

- [x] 3.1 Добавить обязательный `retrieval_mode`, `cached_embeddings` и `new_embeddings` в `ReviewResult`, JSON schema и Markdown warning для `lexical_fallback`; проверить golden-тестами оба режима, корректные counters и запрет evidence IDs, отсутствующих в фактически переданном контексте.
- [x] 3.2 Передавать `review.json` в publisher и добавлять retrieval mode и cache counts к Actions Summary без вывода provider diagnostics или cache contents; проверить Node-тестами create/update sticky comment, `embedding`, `lexical_fallback`, counters и отклонение неизвестного значения.
- [x] 3.3 Синхронизировать Responses payload review-adapter для reasoning-моделей `gpt-5.1`–`gpt-5.6`, добавить безопасные поля provider error и регрессионные тесты, исключающие body/message/prompt/credentials из diagnostics.
- [x] 3.4 Добавить проверяемый `change_summary` с `diff:` evidence и типизированную безопасную причину lexical fallback в JSON/Markdown; покрыть prompt parsing, validation, aggregation, rendering и retryable error classification тестами.
- [x] 3.5 Разделить 30-секундный timeout embedding HTTP-попытки и 5-минутный общий бюджет индексации, сохранять каждый готовый batch до следующего await и проверить partial cache после отмены.
- [x] 3.6 Требовать и валидировать русский текст всех пользовательских полей, один раз повторять нерусский generation-ответ и не публиковать второе нарушение; русифицировать статические подписи Markdown и покрыть оба пути тестами.
- [x] 3.7 Удалить эвристический language-validator и повторный generation-запрос, сохранив требование русского языка в trusted prompt и структурную валидацию; проверить одним вызовом с русским текстом и техническими английскими словами.

## 4. Упрощение GitHub Actions

- [x] 4.1 Переключить review job на `cargo build --release --package fox-review --bin fox-review` и запуск выделенного binary; проверить workflow regression-тестом точную команду, checkout `base.sha`, отсутствие head-code execution и сохранение минимальных permissions.
- [x] 4.2 Добавить в trusted review job restore/save `.ai-review-cache/review-rag.db` с key по schema, runner OS, base branch и безопасному hash provider/model/chunker, уникальным save suffix и сравнением SHA-256 до/после review; проверить workflow-тестом стабильный restore prefix, save только после успешного изменения закрытой SQLite и отсутствие cache steps в test job.
- [x] 4.3 Удалить `fork-notice`, сохранив same-repository guards на `tests` и `review`; проверить статическим тестом отсутствие notification job, provider credentials/cache write в test job и любых выполняющихся jobs для fork PR.
- [x] 4.4 Обновить README и `reports/day32/README.md`: минимальная сборка, cache reuse/invalidation, retry/fallback semantics, retrieval marker и новая fork policy; проверить, что документация не содержит credentials и не обещает отдельный fork summary.
- [x] 4.5 Добавить отдельный trusted Cargo build cache для registry/git и `target/release`, привязанный к OS и `Cargo.lock`; проверить workflow-тестом наличие cache только в review job и неизменность минимальной build-команды.
- [x] 4.6 Явно задать `cache-mode: write` только trusted review job и `cache-mode: read` test job; проверить workflow regression-тестом, что head-код не получает write-capable cache token.
- [x] 4.7 Добавить trusted main workflow для прогрева registry/git и `target/debug` через `cargo test --workspace --no-run`, а в PR test job — только restore того же ключа по OS/Rust/Cargo.lock; проверить отсутствие save action в PR test job.
- [x] 4.8 Сохранять существующий изменившийся RAG cache независимо от результата generation/publish, сохранив запрет публикации неполного комментария; проверить workflow regression-тестом outputs, порядок и условия detect/save.

## 5. Итоговая проверка

- [x] 5.1 Выполнить `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, `node --test .github/scripts/publish-review.test.js` и `openspec validate harden-ai-review-workflow --strict`; исправить все найденные ошибки.
- [x] 5.2 Выполнить последовательные controlled same-repository PR smoke tests: cold cache, warm cache и ответ `429` до fallback; подтвердить reuse неизменившихся vectors, успешную публикацию одного sticky-комментария, режимы `embedding`/`lexical_fallback`, отсутствие secrets/cache contents в логах и отсутствие `fork-notice`, затем записать обезличенный результат в отчёт дня 32.
