## Why

RAG-пайплайн Недели 6 уже хранит structural-индекс в SQLite и выполняет vector search локально, а День 26 добавил Ollama как generation-провайдер. Для Дня 28 нужно соединить эти готовые части без переиндексации: подтвердить, что существующий индекс работает с локальной LLM, и воспроизводимо сравнить её ответы с облачной моделью.

## What Changes

- Сохранить без изменений `.fox-index.db`, embedding-модель `text-embedding-3-small`, dimensions и текущую `.fox-embeddings.json`; новые embeddings и отдельный индекс не создаются.
- Использовать существующий `/rag on`: при выбранной Ollama query rewrite, основной grounded-ответ и возможный repair выполняются локальной `qwen3.5:4b`, а retrieval читает structural-чанки из индекса Недели 6 и считает cosine similarity локально.
- Не добавлять `/rag local on`, local-only guard, новый embedding adapter или новый evaluation command.
- Подтвердить provider-neutral RAG-path регрессионными тестами и исправить только выявленные несовместимости Ollama с текущим grounded-response contract.
- Запустить интерактивный CLI с `--dump-metrics`, задать сохранённый набор вопросов прошлой недели в отдельной Ollama-сессии и, если облачная модель уже настроена, повторить те же вопросы в отдельной облачной сессии.
- Проанализировать сохранённые вопросы, ответы, источники и цитаты вместе со связанными по `session_id` записями `fox-metrics.log`; оценить качество, скорость и стабильность без отдельного evaluation runner.
- Явно зафиксировать границу локальности: SQLite retrieval и генерация Ollama локальны, но embedding текущего вопроса продолжает использовать существующий endpoint из `.fox-embeddings.json`, поэтому решение не заявляется как полностью offline без дополнительной переиндексации.

## Capabilities

### New Capabilities

- `local-rag-execution`: Использование локальной Ollama для query rewrite, grounded generation и repair поверх неизменного индекса Недели 6 и существующего локального vector search.
- `rag-provider-comparison`: Воспроизводимый протокол сравнения сохранённых интерактивных Ollama- и cloud-сессий с метриками их ходов.

### Modified Capabilities

- Нет. Изменение интегрирует существующие `rag-query`, `document-indexing`, `rag-chat-evaluation` и `local-llm-provider` без изменения их публичных контрактов.

## Impact

- Основное изменение — интеграционная проверка и отчёт. Производственный код меняется только при обнаружении фактической несовместимости Ollama с существующим RAG-path.
- `.fox-index.db`, его SQLite-схема, опубликованные embeddings и `.fox-embeddings.json` не мигрируются и не перезаписываются.
- Используются существующие `/rag on`, SQLite-сессии, `--dump-metrics`, retrieval, grounded-answer parser и provider configuration; `rag-eval`, новый CLI и новые Rust-зависимости не требуются.
- Для воспроизводимого запуска нужна локальная Ollama с `qwen3.5:4b`; облачная модель используется только во втором, явно выбранном сравнительном запуске.
- В `reports/day28/` добавляется безопасный анализ выбранных сессий и агрегированных метрик без копирования базы сессий, `fox-metrics.log`, API keys, embedding vectors, content hashes, полных retrieval-чанков и локальных конфигураций.
