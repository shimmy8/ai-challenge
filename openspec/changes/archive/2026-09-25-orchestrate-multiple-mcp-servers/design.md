## Context

См. [proposal.md](proposal.md) — Why. Сейчас `McpConfig`, `McpRuntime` и `/mcp` предполагают один endpoint; `Agent::execute_pipeline_call` одновременно валидирует план, подтверждает действия и исполняет шаги; один `EchoServer` регистрирует все предметные tools; `SchedulerJob` и `DigestRunner` жёстко описывают Calendar → LLM → Telegram. При этом transport-neutral `ToolDefinition`, `ToolCall`, `ToolResult` и линейная модель pipeline уже дают основу для общего исполнителя.

Все MCP handlers, фоновые процессы и provider-neutral orchestration-типы должны оставаться в `src/mcp/`. Секретные env-файлы не читаются и не участвуют в проектировании; каждый предметный сервер получает только собственные локальные credentials.

## Goals / Non-Goals

**Goals:**

- Сделать серверную принадлежность инструмента явной и однозначной от model tool call до `tools/call`.
- Использовать одинаковую семантику pipeline в интерактивном и фоновом режимах.
- Позволить scheduler выполнять любые заранее подтверждённые source → transform → sink цепочки.
- Изолировать внешние интеграции и их секреты по отдельным MCP-процессам.
- Получить проверяемую трассу длинного flow в памяти, истории запусков и stdout.

**Non-Goals:**

- Миграция прежнего одиночного MCP-конфига, старых scheduler-таблиц и существующих календарных заданий.
- DAG, условные ветвления, циклы, параллельные шаги, distributed locks и гарантии exactly-once между процессами.
- Автоматический запуск или supervision всех MCP-серверов из интерактивного клиента.
- Произвольный Telegram recipient, выдача MCP-серверам интерактивной памяти или автоматический rollback внешних действий.

## Decisions

### 1. Registry replaces the single endpoint

`McpConfig` хранит `servers: Vec<McpServerConfig>`, где запись содержит `id`, `url` и `disabled_tools`. ID валидируется до подключения и не содержит `__`; адрес проходит HTTP(S) validation, handshake и `tools/list` до сохранения. Allowlist удаляется: свежий tool включён, если его нативного имени нет в denylist.

`/mcp` сначала показывает обзор реестра, затем операции над выбранной записью: add, reconnect/inspect, edit disabled tools, replace URL и remove. Отказ одной проверки не прерывает обзор остальных серверов.

Альтернатива — сохранить allowlist — безопаснее при неожиданном расширении чужого сервера, но противоречит выбранному для учебного проекта поведению «включено по умолчанию». Подтверждение write tools и fail-closed routing остаются второй линией защиты.

### 2. A router owns sessions and qualified definitions

Новый `McpRouter` реализует `ToolExecutor` и владеет:

- `sessions: BTreeMap<ServerId, McpSession>`;
- `routes: BTreeMap<QualifiedToolName, ToolRoute>`;
- снимком исходных schemas и annotations.

При подключении router строит публичное имя `server_id__native_name`. `definitions()` возвращает копии определений с публичными именами. `execute()` повторно находит route, заменяет имя вызова на нативное, отправляет его только в соответствующую сессию и восстанавливает исходный call ID в результате. `split_once("__")` не используется как единственный источник истины: выполнение разрешается только по построенной таблице routes.

Клиент стартует в degraded mode: успешно подключённые серверы доступны, ошибки остальных выводятся как предупреждения. Это предпочтительнее общего отказа, потому что серверы независимы; конкретный вызов и сохранённый job всё равно завершаются fail-closed, если нужного route нет.

Альтернативы: требовать глобально уникальные нативные имена (конфликты неизбежны) или ставить отдельный gateway перед прежним runtime (добавляет процесс и скрывает маршрутизацию от клиента).

### 3. Local MCP handlers are split by trust boundary

CLI принимает `--mcp-server <kind> --addr <loopback-address>` и создаёт один handler:

- `github`: `repository_metadata`, `project_activity`;
- `reporting`: `calculate_github_metrics`, `render_github_report`;
- `workspace`: `save_report` и настроенный root;
- `calendar`: `list_events`, `create_event`;
- `ai`: `generate_text`;
- `telegram`: `send_message` в фиксированный локальный chat;
- `orchestrator`: lifecycle generic jobs.

Общий transport/bootstrap остаётся переиспользуемым, но tool routers и зависимости handler различаются. `echo`, `EchoRequest` и его тесты удаляются. Разделение по процессам позволяет не загружать CalDAV credentials в GitHub/Reporting, Telegram token в Calendar/AI и writable directory в остальные серверы.

### 4. PipelineExecutor moves into `src/mcp/pipeline.rs`

Из `Agent::execute_pipeline_call` выделяется общий executor с входами:

- неизменяемый `PipelinePlan`;
- snapshot definitions/routes;
- runtime JSON (`run` пуст в интерактивном режиме);
- `ExecutionPolicy`;
- callbacks для approval и invariant verification;
- correlation context.

Executor выполняет общие стадии validate → resolve references → validate resolved schema → authorize → call → trace. Он строго последователен, не делает retry и после первой ошибки заполняет оставшиеся шаги как skipped.

`InteractivePolicy` проверяет инварианты и подтверждает каждый write-step после подстановки. `ScheduledPolicy` принимает только pipeline, ранее подтверждённый как аргумент `orchestrator__create_job`, сверяет сохранённый canonical hash и не открывает интерактивные prompt. Любое изменение создаёт новое задание; отдельной операции редактирования нет.

Ссылки поддерживают полный `step.output`, вложенный путь внутри output и поля `run.id`, `run.trigger`, `run.scheduled_at`. Разрешение сохраняет JSON-типы. Синтаксис и traversal реализуются одним resolver, а не строковыми заменами.

### 5. Orchestrator persists schedules, immutable plans and safe traces

SQLite создаётся заново с общей моделью:

- `jobs`: name, schedule JSON, pipeline JSON, canonical hash, status, next run;
- `runs`: job, trigger, scheduled/start/finish timestamps, status, safe error;
- `run_steps`: ordinal, step ID, qualified/server/native tool names, status, duration, safe error и опционально разрешённый к хранению output.

Календарные `horizon_hours`, `target_day`, `provider`, `model`, `text_source` и `delivered_text` удаляются из общей схемы. Для данных, которые нужно видеть в истории, step может объявить `capture_output: true`; этот выбор входит в подтверждаемую структуру, а сохранённый output ограничивается размером. По умолчанию outputs нужны только в памяти текущего запуска и не сохраняются.

Orchestrator загружает тот же registry, исключает собственный server ID и при каждом запуске строит актуальный router/snapshot. Запрещены шаги `orchestrator__*` и встроенный pipeline planner, поэтому job не может создавать jobs или рекурсивно запускать orchestration.

Атомарный SQLite claim, single-process scheduler, catch-up не более одного запуска и перевод незавершённых runs в interrupted сохраняются из текущей реализации.

### 6. AI and Telegram become ordinary transform and sink tools

`ai__generate_text` принимает инструкции и JSON input, строит отдельный provider request без истории, memory, MCP definitions и tool execution. Provider/model выбираются из локальной конфигурации AI-сервера; аргументы не могут передавать API key. Ответ с tool call или пустым текстом является ошибкой.

`telegram__send_message` принимает только text. Token и chat ID читаются локально Telegram-сервером. Результат имеет outcome `delivered`, `failed` либо `unknown`; unknown никогда не повторяется автоматически.

Provider-neutral `ToolResult` получает явный outcome, чтобы executor отличал подтверждённый успех, ошибку и неопределённый внешний эффект без разбора произвольного текста. Pipeline прекращается и сохраняет unknown при неопределённом результате.

Альтернатива — оставить LLM и Telegram специальными шагами scheduler — проще, но снова связывает orchestration с конкретными transform и sink.

### 7. Calendar provides a composable source

`calendar__list_events` оборачивает существующее безопасное CalDAV-чтение. Он принимает абсолютный `[from,to)` либо ограниченный relative request (`relative_to`, `target_day`, timezone), вычисляет абсолютный интервал внутри Calendar-сервера и возвращает нормализованные события. Так scheduler не получает предметную date arithmetic и не знает о CalDAV.

Calendar digest становится сохранённым pipeline Calendar → AI → Telegram. GitHub report становится GitHub → Reporting → Workspace. Оба сценария используют один executor и демонстрируют разные источники и sinks.

### 8. Logging is metadata-only and correlated

Router создаёт correlation ID на top-level call/run и передаёт server, tool и step metadata в logging context. Локальный handler печатает start/finish/error строки с elapsed milliseconds в stdout отдельного серверного процесса; клиентский router использует silent sink, чтобы не смешивать диагностический JSONL с пользовательской выдачей агента. Аргументы, outputs и HTTP headers не логируются. Для фонового flow добавляются job/run IDs; для интерактивного — call ID.

Тесты перехватывают sink логов через небольшой writer abstraction, а production writer направляет строки в stdout. Это позволяет проверить порядок и redaction без глобальной подмены stdout.

## Risks / Trade-offs

- [Автоматически включённый новый tool расширяет возможности агента] → write annotations всё равно требуют подтверждения, серверы считаются доверенными, denylist доступен сразу после discovery.
- [Имя `server__tool` может превышать ограничения provider] → валидировать полное имя при discovery и не публиковать несовместимое определение, показывая предупреждение.
- [Предварительно разрешённый sink получает динамический текст] → preview явно показывает места `$ref`, destination фиксирован локально, а любое изменение структуры требует нового подтверждения.
- [Схема инструмента меняется после создания job] → выполнять свежий discovery и schema validation на каждом запуске; несовместимый job завершается failed.
- [Несколько процессов усложняют ручной запуск] → документировать фиксированный демонстрационный набор команд и разрешить явные loopback-адреса.
- [Полные outputs могут содержать личные данные] → не сохранять их по умолчанию, ограничивать opt-in capture и никогда не включать payload в stdout.
- [Локальный scheduler не обеспечивает high availability] → сохранить явную single-process границу и честно отражать missed/interrupted runs.
- [Breaking schema оставит старые локальные данные] → использовать новый файл/таблицы либо требовать явного удаления ignored dev database; не пытаться интерпретировать старые rows.

## Migration Plan

1. Ввести новые config types и CLI server selection, не читая прежний MCP singleton.
2. Разделить handlers и добавить отдельные Calendar source, AI, Telegram и Orchestrator servers.
3. Ввести router и переключить интерактивного агента на qualified tools.
4. Выделить общий PipelineExecutor и подтвердить parity существующими pipeline-тестами.
5. Создать новую generic scheduler schema и подключить ScheduledPolicy.
6. Удалить calendar-specific scheduler tools, runner и `echo` после переноса сценариев.
7. Добавить end-to-end тесты и инструкции ручного запуска всех процессов.

Откат выполняется возвратом к предыдущей версии кода и предыдущим локальным config/database-файлам, если пользователь сохранил их отдельно. Новая конфигурация и scheduler database не обязаны читаться старой версией.
