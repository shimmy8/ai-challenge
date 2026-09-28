## Context

См. мотивацию в `proposal.md`. Сейчас `app::run` выбирает между интерактивным режимом и MCP-сервером через `StartupMode`, chat credentials и модели хранятся в общем `Config`, а SQLite применяется для сессий и scheduler. Специализированного document pipeline, независимой embedding-конфигурации и векторного индекса нет. Корпус дня 21 состоит из 37 Markdown-файлов в `reports/` и `openspec/specs/` (на момент проектирования около 33,6 тыс. слов) и достаточно мал, чтобы индексировать его пакетно без отдельного vector database.

Решение должно соблюдать существующие границы ответственности: `main.rs` остаётся тонким, MCP-код остаётся в `src/mcp/`, секреты не копируются в новый индекс, а тесты не обращаются к сети.

## Goals / Non-Goals

**Goals:**

- построить повторяемый конвейер `discover -> load -> chunk -> embed -> persist -> compare`;
- сделать стратегии взаимозаменяемыми и сравнивать их на одном нормализованном снимке корпуса;
- отделить внешний embedding API от детерминированной логики и тестов;
- отделить embedding provider, endpoint, model и credentials от `.fox-llm.json`, `Agent` и chat-provider lifecycle;
- оставить сохранённые данные пригодными для последующего cosine retrieval без миграции формата чанков;
- обеспечить атомарное обновление и дешёвые повторные запуски.

**Non-Goals:**

- универсальный ingestion framework для PDF, HTML, DOCX и бинарных форматов;
- ANN-поиск, FAISS, ранжирование, сборка prompt и генерация RAG-ответа;
- синхронизация индекса между машинами и фоновое наблюдение за файловой системой;
- точный локальный подсчёт OpenAI-токенов до запроса;
- изменение интерактивной модели, памяти, сессий или MCP-инструментов.

## Decisions

### 1. Отдельный startup-режим и модуль `rag`

`StartupMode` получит вариант `Index(IndexOptions)`. Синтаксис будет начинаться с `index`; парсер обработает `--source`, `--strategy`, `--index-path`, `--chunk-size`, `--chunk-overlap` и необязательный `--comparison-output`, после чего `app::run` перед инициализацией интерактивного UI передаст управление в `rag::run_indexing`. `--source` принимает файл или каталог внутри текущего корня проекта и может повторяться. При отсутствии источников используются `reports/` и `openspec/specs/`; при отсутствии стратегии используется `all`. Путь embedding-конфигурации намеренно отсутствует в CLI: индексатор всегда читает `.fox-embeddings.json` из корня проекта.

Код разместится в `src/rag/`:

```text
src/rag/
  mod.rs          orchestration and public domain types
  config.rs       isolated embedding configuration and provider factory
  documents.rs    discovery, loading, normalization
  chunking.rs     fixed and structural strategies
  embeddings.rs   provider trait and OpenAI adapter
  index.rs        SQLite schema, cache lookup, atomic publish
  metrics.rs      comparison statistics and rendering
```

Это сохраняет `src/mcp/` только для MCP runtime и серверов. Альтернатива в виде slash-команды отклонена: индексация является воспроизводимой batch-операцией и не должна запускать редактор, агента, модели диалога или MCP connections.

### 2. `.fox-embeddings.json` является единственным источником embedding-настроек

Новый `EmbeddingConfig` загружается только внутри `rag::run_indexing`; обычный интерактивный и MCP startup не открывают этот файл. Формат использует `serde(deny_unknown_fields)` и содержит всё, что требуется adapter:

```json
{
  "provider": "openai-compatible",
  "endpoint": "https://api.openai.com/v1/embeddings",
  "model": "text-embedding-3-small",
  "dimensions": 1536,
  "batch_size": 64,
  "api_key": "replace-with-real-key-in-local-file"
}
```

Loader требует непустые provider/endpoint/model, HTTP(S) endpoint и положительные dimensions/batch size. `api_key` обязателен только adapter, которому нужна bearer-авторизация; локальный compatible endpoint может использовать пустое значение. Локальный файл создаётся с Unix mode `0600`, исключается из Git и никогда не сериализуется в метрики или индекс. Коммитируемый `fox-embeddings.example.json` содержит placeholder, а не рабочий ключ.

Provider factory первой версии знает `openai-compatible`. Для перехода с OpenAI на совместимый локальный сервис пользователь меняет endpoint/model/dimensions/api_key только в `.fox-embeddings.json`. Для нативного протокола будущая реализация добавит ещё один adapter и ветку factory внутри `src/rag/`; `Agent`, `Provider`, `AgentSettings`, `.fox-llm.json` и `providers.rs` не меняются.

Не предусмотрен `--embedding-config`: единый путь упрощает защиту секретов и исключает случайную загрузку файла из недоверенного места. Не предусмотрен fallback на `Config::key(Provider::Openai)`, потому что он снова связал бы индексатор с агентским flow.

В `.gitignore` добавляется `.fox-embeddings.json`. В `AGENTS.md` добавляется прямой запрет агентам читать, индексировать, анализировать, цитировать или выводить этот файл и его credentials; для документации, тестов и fixtures разрешён только `fox-embeddings.example.json` с фиктивными значениями.

### 3. Один нормализованный снимок документов для обеих стратегий

Discovery рекурсивно обходит только `.md`, канонизирует и сортирует пути, устраняет дубликаты и отклоняет выход за корень проекта. Загрузка нормализует CRLF в LF, удаляет UTF-8 BOM и не меняет остальное содержимое. Путь в метаданных хранится относительно корня с `/` как разделителем. Заголовок берётся из первого H1, иначе из stem файла.

Обе стратегии получают один и тот же `Document`, поэтому различия метрик относятся к chunking, а не к чтению файлов. Рекурсивный обход реализуется стандартной библиотекой: добавлять `walkdir` для небольшого дерева нет необходимости.

### 4. Fixed и structural используют общий размерный контракт

Размер измеряется Unicode scalar values, а не байтами, чтобы русский текст не получал искусственно меньшие чанки. Значения по умолчанию: `chunk_size=1200`, `chunk_overlap=200`.

`fixed` идёт по документу последовательно. Перед жёсткой границей он ищет последний двойной перевод строки, затем перевод строки; если подходящей границы нет, режет точно по символам. Следующее окно начинается с заданным overlap. Алгоритм гарантирует прогресс и удаляет только пустые края чанка.

`structural` сначала строит секции по Markdown heading events и offset ranges через `pulldown-cmark`. Путь секции представлен цепочкой заголовков, например `Task lifecycle > Validation`. Преамбула получает секцию `document`. Раздел размером не больше лимита становится одним чанком; большой раздел проходит через тот же ограничитель размера, что и fixed, не смешиваясь с соседними разделами.

Общий ограничитель делает размерное сравнение честным. Простое регулярное выражение для строк `#` отклонено, поскольку оно неправильно распознаёт заголовки внутри fenced code blocks и setext headings.

### 5. Идентификаторы и кеш основаны на SHA-256

Добавляется `sha2`. `content_hash` вычисляется из нормализованного текста чанка. `chunk_id` вычисляется из версионированной канонической записи: source, strategy, параметры chunking, ordinal, section и content hash. Hex-строка детерминирована между процессами и платформами.

`chunk_id` сохраняет происхождение одинакового текста из разных документов, а `content_hash` позволяет переиспользовать один embedding между такими чанками и между стратегиями. `DefaultHasher` отклонён, поскольку его формат не является долговременным контрактом.

### 6. Provider-neutral embeddings с последовательными batch-запросами

Внутренний trait `EmbeddingProvider` принимает срез текстов и возвращает в исходном порядке vectors, model, dimensions и usage tokens. Factory строит production-adapter исключительно из `EmbeddingConfig`. OpenAI-compatible adapter вызывает настроенный endpoint через существующий `reqwest`, передаёт model, `encoding_format=float`, dimensions и bearer API key из `.fox-embeddings.json`, если ключ непустой. Новые тексты группируются по настроенному `batch_size` и отправляются последовательными запросами: этого достаточно для текущего корпуса и проще для обработки rate limits и атомарности.

Перед запросами из SQLite выбираются совместимые embeddings по `(content_hash, model, dimensions)`. В API уходят только cache misses. Ответ каждой партии полностью валидируется: количество и индексы vectors, единая размерность и конечные `f32`. Все новые результаты удерживаются в памяти до единой транзакции; ожидаемый объём корпуса составляет лишь несколько мегабайт.

Trait реализуется fake provider в тестах. OpenAI SDK не добавляется, потому что проект уже использует `reqwest` и endpoint требует небольшого JSON-контракта. Встроенный runtime локальной модели отклонён для дня 21 из-за размера runtime и весов; локальный OpenAI-compatible service поддерживается конфигурацией, а иной протокол добавляется новым adapter без влияния на агент.

### 7. Отдельный SQLite-индекс и атомарная публикация

`.fox-index.db` не смешивается с `.fox-sessions.db`: это воспроизводимый производный артефакт с независимым жизненным циклом. База использует WAL, foreign keys и права `0600` на Unix. Основные таблицы:

```text
index_runs(id, created_at, strategy_set, options_json,
           comparison_json, api_tokens)
documents(source, title, document_hash, byte_len, char_len)
embeddings(content_hash, model, dimensions, vector_blob,
           PRIMARY KEY(content_hash, model, dimensions))
chunks(chunk_id, strategy, source, ordinal, title, section,
       content, content_hash, model, dimensions, run_id)
```

`vector_blob` содержит little-endian `f32`; чтение проверяет `blob.len() == dimensions * 4`. Foreign keys связывают chunk с document, embedding и successful run.

После успешных API-вызовов открывается одна транзакция: upsert документов и embeddings, удаление только текущих chunks выбранных стратегий, вставка новых chunks и запись run metrics. Для `all` обе стратегии публикуются вместе. Ошибка до commit оставляет предыдущий индекс неизменным; кеш embeddings намеренно не очищается при замене чанков.

JSON-файл рассматривался как более наглядный, но отклонён из-за дублирования больших float arrays, отсутствия транзакций и неудобного кеш lookup. FAISS не нужен до появления retrieval и существенно усложнил бы сборку Rust-проекта.

### 8. Сравнение строится до публикации и сохраняется с run

Для каждой стратегии вычисляются document/chunk count, min/max/mean/median Unicode-char length, section coverage, new/reused embedding count и elapsed milliseconds. API usage tokens суммируются только для реально отправленных партий; денежная оценка не вычисляется и не входит ни в конфигурацию, ни в сравнение.

CLI печатает компактное русскоязычное сравнение, а полный JSON сохраняется в `index_runs.comparison_json`. Если задан `--comparison-output`, тот же sanitized JSON после SQLite commit записывается во временный файл рядом с целью и атомарно переименовывается. Экспорт не содержит vectors, chunk texts, API key или полного embedding config. Provider, endpoint origin без credentials, model и dimensions входят в run metadata и отчёт.

`reports/day21/README.md` фиксирует состав корпуса, команды запуска, параметры, полученные метрики и интерпретацию различий. Числа в отчёте берутся из успешного запуска, а не задаются в коде или тестах.

### 9. Построчный progress в stderr и сетевой timeout

`index` всегда пишет безопасные русскоязычные статусы этапов в `stderr`, чтобы `stdout` оставался пригодным для итоговой таблицы и перенаправления. Вывод охватывает discovery, chunking, cache lookup, каждую embedding-партию, SQLite transaction и comparison export. Перед `await` сетевого ответа печатаются номер партии, общее число партий и количество текстов; после ответа — накопительное число vectors, usage tokens партии и elapsed time. При полном cache hit embedding-строки не имитируются: команда явно сообщает ноль cache misses и переходит к публикации.

Progress реализуется через небольшой writer/reporter, который production-код направляет в `stderr`, а тесты — в буфер. Это позволяет проверять порядок и sanitized-состав строк без глобального перехвата процесса и без зависимости для progress bar. Динамическая перерисовка одной строки отклонена: построчный вывод одинаково понятен в интерактивном терминале, CI и сохранённом логе. Отдельный `--verbose` не добавляется, поскольку отсутствие признаков жизни было проблемой поведения по умолчанию.

`reqwest::Client` для embedding adapter получает timeout 60 секунд на запрос. Timeout обрабатывается как обычная безопасная ошибка провайдера: этап и номер партии видны, но URL с query, request body, заголовки и credentials не печатаются; SQLite publish не начинается до успешного завершения всех партий.

### 10. Проверка без сетевой зависимости

Unit tests используют `tempfile`, небольшие Markdown fixtures и fake provider с детерминированными vectors и usage. Они проверяют Unicode-границы, overlap, fenced code headings, hierarchy, oversized sections, stable IDs, source isolation, cache hits, BLOB round-trip, частичный cache miss и rollback. HTTP parsing тестируется синтетическими JSON-ответами без API key.

Отдельные тесты startup parser гарантируют, что существующие `--dump-metrics` и `--mcp-server` не меняют семантику. Config tests работают только с временными файлами и фиктивными ключами. Живая индексация полного корпуса выполняется как ручная проверка с локально настроенным `.fox-embeddings.json` и ожидаемым расходом менее одного цента; агент не читает этот файл, а секрет не печатается тестами или отчётом.

## Risks / Trade-offs

- [Символьный размер не равен token size] -> 1200 символов с большим запасом укладываются в лимит embedding model; фактические billable tokens брать из API usage.
- [Structural strategy создаст очень маленькие чанки для частых заголовков] -> не смешивать семантически разные разделы ради метрики; явно показывать min/median и обсудить эффект в отчёте.
- [Изменение раннего текста сдвинет ordinal последующих fixed chunks] -> считать новый набор корректной новой версией, но переиспользовать embedding по content hash там, где текст совпал.
- [Batch завершится частично на стороне API] -> не публиковать локальные данные до успешного завершения всех партий; повторный запуск может повторно оплатить только неподтверждённую работу.
- [SQLite cache со временем накапливает orphan embeddings] -> принять малый рост для учебного корпуса; garbage collection отложить до появления эксплуатационной необходимости.
- [Документы могут содержать чувствительный текст] -> индекс остаётся локальным с правами `0600`, CLI явно сообщает о внешней отправке новых чанков, а диагностический вывод не содержит тела запросов.
- [`.fox-embeddings.json` содержит API key] -> применять mode `0600`, Git ignore, прямой запрет в `AGENTS.md` и использовать в тестах и документации только безопасный example.
- [Локальный provider реализует несовместимый протокол] -> первая версия гарантирует config-only switching для OpenAI-compatible endpoints; иной протокол добавляется отдельным adapter внутри `src/rag/` без изменений Agent flow.

## Migration Plan

1. Добавить `.fox-embeddings.json` и `.fox-index.db*` в `.gitignore`, безопасный `fox-embeddings.example.json` в репозиторий и запрет чтения локального embedding config в `AGENTS.md`; существующий `.fox-llm.json` не мигрировать.
2. Добавить новый режим, isolated config loader и provider factory, не меняя agent/chat configuration.
3. Создать индекс новой схемы при первом `index`-запуске и заполнить его в одной транзакции.
4. Проверить обе стратегии на fake provider, затем выполнить один живой запуск полного корпуса с локально настроенным `.fox-embeddings.json`.
5. При откате удалить новый код, example и, при необходимости, локальные `.fox-embeddings.json` и `.fox-index.db*`; существующие сессии, scheduler и `.fox-llm.json` останутся совместимыми.
