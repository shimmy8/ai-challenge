## 1. CLI и каркас RAG

- [x] 1.1 Добавить зависимости `pulldown-cmark` и `sha2`, объявить `src/rag/` в тонком `main.rs`, исключить `.fox-index.db*` и `.fox-embeddings.json` из Git, добавить безопасный `fox-embeddings.example.json` и запрет чтения локального embedding config в `AGENTS.md`; проверить `cargo build`, отсутствие рабочего credential в example и `git status --ignored`.
- [x] 1.2 Реализовать `IndexOptions`, варианты стратегии и разбор `fox-llm index` с повторяемым `--source`, `--strategy`, `--index-path`, `--chunk-size`, `--chunk-overlap`, `--comparison-output` и без параметра пути embedding config; проверить unit-тестами defaults, все допустимые комбинации, неизвестные аргументы и недопустимый overlap.
- [x] 1.3 Подключить `StartupMode::Index` в `app::run` до интерактивной и MCP-инициализации; регрессионными тестами подтвердить неизменное поведение пустого запуска, `--dump-metrics` и `--mcp-server`.

## 2. Загрузка документов и chunking

- [x] 2.1 Реализовать детерминированное рекурсивное обнаружение Markdown-файлов, защиту границы корня, устранение дублей, LF/BOM-нормализацию и извлечение title; проверить fixtures для каталогов, отдельных файлов, сортировки, H1/fallback title, пустого и недоступного корпуса.
- [x] 2.2 Реализовать общий Unicode-aware ограничитель и fixed chunking с предпочтением границ абзаца/строки и overlap; проверить русскоязычный текст, отсутствие пустых чанков, максимальный размер, точный прогресс и короткий документ.
- [x] 2.3 Реализовать structural chunking через offset events `pulldown-cmark`, иерархические section paths, preamble `document` и fallback для большого раздела; проверить ATX/setext headings, fenced code blocks, вложенные и oversized sections.
- [x] 2.4 Добавить канонические метаданные, SHA-256 `content_hash` и версионированный `chunk_id`; проверить стабильность повторного запуска и разные IDs для одинакового текста из разных источников.

## 3. Embedding provider

- [x] 3.1 Реализовать отдельный `EmbeddingConfig`, который загружает только `.fox-embeddings.json`, валидирует provider/HTTP(S) endpoint/model/dimensions/batch size и создаёт файл с mode `0600`; fixtures проверить valid OpenAI, local endpoint, отсутствие файла, неизвестные поля/provider и все недопустимые значения без чтения `.fox-llm.json`.
- [x] 3.2 Определить provider-neutral `EmbeddingProvider`, provider factory, типы batch response/usage и детерминированный fake provider; проверить выбор adapter, сохранение порядка, модели, размерности и суммирование usage без сети или Agent dependencies.
- [x] 3.3 Реализовать OpenAI-compatible adapter на `reqwest`, полностью настраиваемый через `EmbeddingConfig`; проверить синтетическими ответами endpoint/model/dimensions/batch size/bearer key, работу локального endpoint без ключа и отклонение HTTP-ошибок, неверного количества vectors, разных dimensions и non-finite значений.
- [x] 3.4 Обеспечить безопасные ошибки и изоляцию startup flows; тестами подтвердить ранний отказ `index` без обязательного ключа, отсутствие key/Authorization/chunk texts в ошибках и то, что повреждённый `.fox-embeddings.json` не читается интерактивным агентом или MCP-сервером.

## 4. SQLite-индекс и оркестрация

- [x] 4.1 Реализовать `IndexStore` со схемой runs/documents/embeddings/chunks, WAL, foreign keys, Unix mode `0600` и little-endian `f32` BLOB; проверить создание временной базы, schema constraints и vector round-trip с валидацией длины.
- [x] 4.2 Реализовать lookup кеша по `(content_hash, model, dimensions)` и планирование только уникальных cache misses; проверить повторное использование между стратегиями, полное попадание и изменение одного документа.
- [x] 4.3 Реализовать атомарную публикацию выбранных стратегий с сохранением другой стратегии и истории успешных runs; fault-injection тестами проверить, что ошибка provider или транзакции оставляет предыдущие chunks доступными без частичного набора.
- [x] 4.4 Собрать `discover -> load -> chunk -> cache -> batch embed -> compare -> publish` в `run_indexing`; end-to-end тестом на временном корпусе и fake provider проверить обе стратегии, повторный запуск без новых provider calls и содержимое SQLite.
- [x] 4.5 Добавить построчный русскоязычный progress по умолчанию в `stderr` для discovery, chunking, cache lookup, каждой embedding-партии, SQLite publish и comparison export; перед запросом показывать `batch N/M` и число текстов, после — накопительный прогресс, tokens и duration. Настроить timeout 60 секунд на embedding-запрос и безопасную ошибку без публикации частичного индекса. Через тестовый writer и fake provider проверить порядок этапов, полный cache hit без фиктивных партий, timeout/failure и отсутствие API key, Authorization, chunk texts, content hashes и полного config в выводе.

## 5. Метрики и отчёт дня 21

- [x] 5.1 Реализовать симметричные метрики размера, section coverage, new/reused embeddings, API tokens и duration; unit-тестами проверить median для чётного/нечётного числа чанков и нулевые API tokens при полном cache hit.
- [x] 5.2 Добавить русскоязычный CLI summary, JSON `comparison_json` в successful run и атомарную sanitized-запись `--comparison-output`; snapshot/структурными тестами подтвердить одинаковый набор полей fixed/structural и отсутствие vectors, chunk texts, embedding config и credentials.
- [x] 5.3 Выполнить индексацию `reports/` и `openspec/specs/` обеими стратегиями с локально настроенным `.fox-embeddings.json`, экспортировать `reports/day21/chunking-comparison.json`, сверить число файлов и API usage, затем оформить `reports/day21/README.md` с командами воспроизведения, фактическими метриками и выводами сравнения без содержимого конфигурации и credentials.

## 6. Итоговая проверка

- [x] 6.1 Запустить `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings` и `cargo test`; исправить все ошибки и указать команды и результаты проверки в отчёте дня 21.
