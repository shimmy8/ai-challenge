## Context

Существующий `fox-llm` уже имеет три нужных механизма, но они пока не соединены в support workflow:

- startup mode разбирается в `metrics.rs`, а `app.rs` направляет процесс в интерактивный CLI, индексатор, evaluation runner или отдельный MCP server;
- `axum` и `rmcp` уже обслуживают streamable HTTP MCP на loopback, а внутренний help runtime показывает паттерн запуска MCP на временном локальном порту;
- RAG умеет работать с явно переданным `index_path`, выполнять enhanced retrieval и проверять grounded attribution, хотя интерактивная обёртка по умолчанию использует `.fox-index.db` и контекст CLI-задачи;
- `Agent::ask_rag_inner` намеренно запрещает tool calls во время grounded generation, поэтому смешивать модельно управляемый MCP-цикл и RAG-генерацию в одном запросе не следует.

См. `proposal.md` для мотивации и delta-specs для поведенческих контрактов. Репозиторное правило требует держать весь MCP-код и его runtime в `src/mcp/`, а `main.rs` оставить тонким.

## Goals / Non-Goals

**Goals:**

- Один процесс и одна команда запускают локальный support HTTP API, веб-страницу и внутренний MCP runtime тикетной базы.
- HTTP-оркестратор детерминированно получает контекст тикета через MCP до retrieval и generation; модель не выбирает тикет и не получает инструменты записи.
- Каждый AI-ход изолирован по `ticket_id`, сохраняет последовательность сообщений и использует только отдельный индекс linkding.
- Компоненты хранилища, MCP, retrieval/generation и HTTP можно тестировать независимо с временными файлами и scripted providers.

**Non-Goals:**

- Публичный production-hosting, регистрация, парольная аутентификация, роли операторов, multi-tenant изоляция и TLS.
- Полный аналог Zammad, SLA, вложения, email-каналы, назначение операторов и административный интерфейс.
- Автоматическая загрузка постоянно меняющейся документации linkding во время запуска.
- Автономное изменение статуса тикета моделью или произвольные model-driven MCP writes.
- Одновременное использование общего проектного RAG-индекса и support-индекса в одном ответе.

## Decisions

### 1. Support-режим является отдельной веткой приложения

`StartupMode` получит вариант `Support(SupportOptions)`, выбираемый командой `fox-llm support`. Минимальные параметры:

- `--addr`, обязательный loopback socket address;
- `--ticket-db`, по умолчанию `.fox-support.db`;
- `--index-path`, по умолчанию `.fox-linkding-index.db`.

`app.rs` передаст управление в новый модуль `src/support/`, не создавая rustyline, `SessionStore` или интерактивный `AgentPool`. Модуль разделяется на HTTP-маршруты, support-оркестратор, типы API и встроенные frontend assets. `main.rs` останется только точкой входа.

Альтернатива — добавить команды `/support` в существующий CLI. Она отвергнута: браузерный жизненный цикл, параллельные HTTP-запросы и отдельное хранилище становятся скрыто связаны с интерактивной сессией.

### 2. Веб-интерфейс и API используют один axum router

Один listener обслуживает:

```text
GET  /                              -> встроенная HTML-страница
GET  /assets/support.css            -> встроенный CSS
GET  /assets/support.js             -> встроенный JS
POST /api/tickets                   -> создать пользователя, тикет и первое сообщение
GET  /api/tickets/{ticket_id}       -> получить карточку и историю
POST /api/tickets/{ticket_id}/messages -> сохранить вопрос и получить AI-ответ
```

Frontend будет написан на обычных HTML/CSS/JavaScript и собран в бинарник через `include_str!`; Node/npm и отдельный dev server не нужны. JS использует только same-origin JSON API. Создание тикета переводит страницу в chat view, а первоначальное описание становится первым пользовательским сообщением и сразу запускает первый AI-ход. После ответа интерфейс позволяет отправлять уточнения тем же endpoint диалога.

Альтернатива — серверный HTML rendering для каждого действия. Она добавляет перезагрузки и дублирует JSON-контракт, не давая пользы учебному single-page сценарию.

### 3. Тикетная БД доступна support-приложению через внутренний MCP runtime

Новые `src/mcp/support.rs` и `src/mcp/support_runtime.rs` будут содержать схему, миграции, MCP handler и lifecycle. При старте support-режима приложение:

1. открывает или мигрирует `.fox-support.db`;
2. поднимает support-only MCP server на свободном loopback-порту;
3. подключает к нему `McpSession`;
4. запускает пользовательский HTTP router;
5. при shutdown закрывает MCP session и останавливает внутренний listener.

Инструменты первой версии:

```text
create_ticket(name, email, subject, description)       [write]
get_ticket(ticket_id)                                  [read-only]
list_ticket_messages(ticket_id, limit, char_budget)    [read-only]
add_ticket_message(ticket_id, role, content)            [write]
```

HTTP POST является прямым пользовательским намерением, поэтому support-оркестратор вызывает write tools с ровно провалидированными аргументами без model approval. LLM не получает определения этих инструментов. Это не ослабляет общий контракт model-driven tool calling: интерактивный агент продолжает запрашивать подтверждение для write tools.

Альтернатива — вызывать `SupportStore` напрямую из HTTP handlers. Она проще технически, но не выполняет требование задания о CRM/JSON через MCP и не проверяет реальный MCP-контракт.

### 4. SQLite-модель минимальна и изолирована

Отдельная версия схемы управляет тремя сущностями:

```text
users(id, name, normalized_email, created_at)
tickets(id, user_id, subject, status, created_at, updated_at)
messages(id, ticket_id, role, content, created_at)
```

`normalized_email` уникален; новый тикет повторно использует пользователя с тем же email. Создание пользователя, тикета и первого сообщения выполняется одной транзакцией. Сообщения сортируются по монотонному `id`, а timestamps используются для отображения, но не для разрешения порядка. Все потенциально блокирующие SQLite-операции выполняются вне async executor либо через ограниченный blocking adapter.

Support DB не регистрируется в `SessionStore` и не содержит LLM prompts, embeddings, citations или tool traces. Ответ ассистента сохраняется только после успешной проверки grounded-контракта.

### 5. Оркестратор, а не модель, собирает контекст хода

После `create_ticket` первоначальное сообщение уже сохранено, и оркестратор запускает для него тот же AI-этап, что и для последующих сообщений. Обработка последующего `POST /api/tickets/{id}/messages` имеет фиксированный порядок:

```text
validate request
      |
      v
MCP get_ticket + bounded history
      |
      v
MCP add user message ------------------------+
      |                                      |
      v                                      | failure retains message id
support query rewrite -> linkding retrieval  |
      |                                      |
      v                                      |
grounded generation + validation             |
      |                                      |
      v                                      |
MCP add assistant message                    |
      |                                      |
      v                                      v
JSON answer + citations              structured partial error
```

Запись вопроса происходит до дорогих сетевых этапов, чтобы тикет отражал фактическое обращение даже при сбое модели. Ошибка после этой точки возвращает `ticket_id`, `message_id` и `assistant_message: null`. Для создания тикета успешная запись ресурса сохраняет HTTP status `201`, а частичный AI-сбой отражается в `outcome` и `error`; для последующего сообщения используется соответствующий error status вместе с признаком `message_saved: true`.

AI-ходы первой версии сериализуются общим async gate. Это ограничивает throughput, но исключает интерливинг двух одновременных ответов и достаточно для локального учебного стенда. При развитии gate можно заменить per-ticket lock без изменения API.

Альтернатива — позволить обычному `Agent` самому вызвать MCP, а затем включить RAG. Она конфликтует с запретом tool calls внутри `ask_rag_inner`, делает выбор тикета недетерминированным и усложняет доказательство изоляции данных.

### 6. Support retrieval использует отдельный адаптер и существующее ядро RAG

Корпус размещается в `reports/day33/knowledge-base/` и содержит:

- выбранный локальный snapshot разрешённых Markdown-документов linkding с manifest upstream version/commit и лицензии;
- написанный для задания FAQ с типовыми вопросами поддержки.

Существующая команда `index` строит его с явными `--source` и `--index-path .fox-linkding-index.db`. Support-режим никогда не использует `DEFAULT_INDEX_FILE`.

Новый support-specific `QueryRewriter` формирует rewrite из исходного вопроса, темы, первоначального описания и ограниченного суффикса пользовательских сообщений. Далее он вызывает существующий `retrieve_enhanced_with_provider(index_path, ...)`. Так cosine search, thresholds, reranking, budgets и проверка совместимости не дублируются, а CLI-specific `RetrievalContext` не превращается в контейнер тикетных полей.

### 7. Grounded generation остаётся без MCP-инструментов

Support assistant создаёт изолированный generation request из четырёх явно разделённых частей:

1. support system instructions;
2. JSON-снимок текущего тикета как недоверенные данные;
3. исходный вопрос пользователя;
4. пронумерованные retrieval chunks как недоверенная документация.

Запрос выполняется с `RequestOptions::default()`, то есть без tool definitions. Существующие builder/parsers grounded-ответа и единственный repair используются повторно. Citations проверяют только документы; сведения из тикета можно упоминать как пользовательский контекст, но нельзя помещать в attribution-блок.

Support history не использует CLI memory, profile, task lifecycle или compression strategies. В prompt попадает ограниченный суффикс сообщений из ticket DB, а полный ticket history остаётся доступен UI.

### 8. Ошибки имеют стабильный JSON envelope

Все API-ошибки используют форму:

```json
{
  "error": {
    "code": "support_index_unavailable",
    "message": "Индекс документации linkding недоступен"
  },
  "ticket_id": "T-...",
  "message_id": 42,
  "message_saved": true
}
```

HTTP status различает validation (`400`), missing ticket (`404`), conflict (`409`) и internal/provider failure (`500`/`503`). Внешние ошибки проходят sanitization и не включают credentials, request bodies, vectors или полные prompts.

## Risks / Trade-offs

- **[Внутренний MCP увеличивает количество движущихся частей]** → переиспользовать lifecycle-паттерн `HelpMcpRuntime`, поднимать listener на loopback и покрыть реальным MCP integration test.
- **[Один AI gate ограничивает параллельность]** → явно принять это для локального demo; отделить gate интерфейсом, чтобы позже заменить per-ticket locking.
- **[Upstream-документация linkding меняется]** → индексировать локальный snapshot с manifest и не скачивать документы во время запуска или тестов.
- **[Тикет может сохраниться без ответа]** → возвращать идентификатор сохранённого сообщения и показывать partial failure в UI; повторный вопрос не теряет историю.
- **[Prompt injection из тикета или документации]** → сериализовать разделы отдельно, маркировать оба источника недоверенными и не передавать tools в model request.
- **[Персональные данные доступны без login]** → ограничить сервер loopback, не поддерживать публичный bind и использовать только фиктивные данные в демо и тестах.
- **[Лицензионные и provenance-риски корпуса]** → хранить только разрешённый минимальный snapshot, включить attribution/license manifest и писать FAQ своими словами.

## Migration Plan

1. Добавить новые игнорируемые локальные файлы `.fox-support.db*` и `.fox-linkding-index.db*`, не меняя существующие базы.
2. Добавить support-схему версии 1; при отсутствующем файле она создаётся автоматически, а неизвестная более новая версия отклоняется.
3. Добавить локальный linkding corpus и построить отдельный индекс документированной командой.
4. Запустить `fox-llm support` с новыми путями и проверить browser flow.
5. Для rollback остановить support-процесс и удалить только новые локальные support DB/index; интерактивный CLI и его данные не затрагиваются.

## Open Questions

- Финальный визуальный стиль страницы можно выбрать при реализации, если он сохраняет перечисленные состояния и не вводит frontend build toolchain.
