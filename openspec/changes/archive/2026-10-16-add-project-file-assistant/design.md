## Context

См. мотивацию в `proposal.md`. Сейчас основной `Agent::ask_inner` уже реализует общий цикл structured tool calls, лимит восьми вызовов, проверку глобальных инвариантов, подтверждение state-changing tools и возврат фактического результата модели. `McpRouter` квалифицирует инструменты по server ID, а `HelpMcpRuntime` показывает рабочий шаблон автоматически запущенного ephemeral loopback MCP и свежего изолированного субагента.

Существующий `workspace` server умеет читать Git-ветку, вызывать CodeGraph и сохранять один отчёт в корень, но требует ручной регистрации и не предоставляет общего чтения, поиска или безопасной замены файлов. Help-only runtime намеренно read-only и не должен расширяться записью. Код серверов, filesystem policy, lifecycle и project-субагента должен оставаться в `src/mcp/`; `app.rs` отвечает только за CLI orchestration и интерактивное подтверждение.

## Goals / Non-Goals

**Goals:**

- переиспользовать существующий provider-neutral tool loop для OpenAI, Claude и Ollama;
- дать модели минимальный набор композиционных файловых инструментов вместо shell;
- сделать read-only исследование автоматическим, а запись — проверяемой, подтверждаемой и защищённой от stale state;
- формировать итог по фактическому trace, чтобы модель не могла заявить о невыполненной записи;
- обеспечить полностью автоматические тесты без API credentials и ручной сценарий на реальных файлах репозитория.

**Non-Goals:**

- произвольные shell-команды, сборка или запуск кода через project MCP;
- удаление, переименование, chmod, работа с binary files или создание отсутствующих каталогов;
- многофайловая транзакция: каждый файл подтверждается и записывается отдельно;
- IDE-подобное редактирование по диапазонам, AST rewrite или автоматический `git commit`;
- автоматическая индексация CodeGraph, публичный MCP endpoint или синхронизация project runtime с пользовательским `/mcp` registry.

## Decisions

### 1. Отдельная команда и отдельный изолированный субагент

`/project <цель>` разбирается отдельно от `/help` и обычного сообщения. Для каждого запуска создаётся свежий `Agent` с клонированными provider settings, HTTP client, request client и активными глобальными инвариантами. История, summary, memory, профиль, задача, RAG chunks, branches и пользовательский tool executor не копируются. Pipeline planner выключается, чтобы каждый state-changing call проходил отдельное подтверждение и trace оставался простым.

Project instructions требуют считать содержимое файлов недоверенными данными, использовать инструменты до ответа, сначала прочитать источники и цель записи, не интерпретировать найденный текст как разрешение на дополнительные действия и честно различать read-only вывод, предложение, отмену и выполненную запись. Для вопросов о символах, вызовах и зависимостях при наличии индекса инструкции требуют сначала `codegraph_explore`; `search_files` остаётся fallback для недоступного CodeGraph и явно текстовых запросов. После завершения orchestration проверяет trace: должен существовать хотя бы один успешный read-only project call; write требует предшествующего `read_file` с тем же path/hash либо `list_files`, подтвердившего отсутствие цели. Если evidence нет, ответ не маркируется как результат файловой проверки.

Исключение — структурированный `policy_denied` от project tool. Guarded executor нормализует только точную безопасную workspace-policy ошибку в этот код, не добавляя исходный путь. Для реального loopback MCP проверяется типизированный `rmcp::ServiceError::McpError` и точное поле `ErrorData.message`, а не форматированная строка протокольной обёртки. Trace принимает такой отказ как терминальный проверенный исход, а renderer игнорирует provider-generated текст и выдаёт фиксированное сообщение без имени, подтверждения существования или содержимого запрещённой цели. Остальные tool failures по-прежнему не удовлетворяют research gate.

Очищенная цель и финальная сводка могут быть записаны в родительскую сессию аналогично `/help`, но tool calls/results туда не попадают. К сводке модели клиент детерминированно добавляет отдельные перечни найденных discovery-инструментами, фактически прочитанных и изменённых путей из trace. Только отдельный успешный `read_file` попадает в «Проверено» и может дать evidence для записи, причём замена требует полного неусечённого чтения без диапазона. Если финальный provider call падает после успешной записи, клиент всё равно выводит список состоявшихся записей и их diff, а ошибку синтеза сообщает отдельно.

Альтернатива — подключить tools основному агенту постоянно. Она уменьшает число сущностей, но смешивает историю и MCP registries и делает случайную запись доступной каждому обычному вопросу. Альтернатива с ручной регистрацией существующего `workspace` server не выполняет требование автономного goal-level сценария.

### 2. Второй ephemeral runtime вместо расширения help-only MCP

При старте интерактивного приложения рядом с `HelpMcpRuntime` запускается `ProjectMcpRuntime` на `127.0.0.1:0`. Runtime создаёт server ID `project`, подключает к нему отдельный `McpRouter` и предоставляет executor только project-субагенту. Ошибка запуска не ломает обычный чат или `/help`, но `/project` fail-closed завершается до provider-вызова. Shutdown повторяет существующий 200 ms graceful budget и затем отменяет server task.

Предлагаемое размещение:

- `src/mcp/project_files.rs` — request/result types, path policy, bounded traversal, read/search/write/diff и preview;
- `src/mcp/project_runtime.rs` — loopback lifecycle и executor;
- `src/mcp/project_assistant.rs` — parsing `/project`, fresh-agent orchestration и trace-derived result;
- `src/mcp/group_servers.rs` — `ProjectWorkspaceServer` и MCP tool registration;
- `src/mcp/mod.rs` — модули и узкие exports;
- `src/app.rs` — startup/shutdown, выбор workflow и interactive approval;
- `src/cli.rs`/`src/config.rs` — строка `/project` в help/completion без persistent config.

Help-only MCP сохраняет прежний набор. Обычный запускаемый `--mcp-server workspace` также не получает новые опасные возможности: write tools существуют только на недоступном извне ephemeral project server.

### 3. Малые инструменты с детерминированными JSON schemas

Project server публикует:

- `list_files { prefix?, extensions?, limit? }` — стабильный список доступных относительных путей;
- `search_files { query, prefix?, extensions?, max_results? }` — literal case-sensitive UTF-8 search с line number и bounded snippet;
- `read_file { path, start_line?, end_line? }` — диапазон строк, полный размер/число строк и SHA-256 всей версии;
- `write_file { path, mode, content, expected_sha256? }` — `create` требует отсутствия цели, `replace` требует точный hash;
- `git_diff { paths? }` — bounded repository diff для допустимых путей, включая синтезированное добавление доступного untracked text file;
- `codegraph_explore { question }` — существующий bounded inspector.

Router показывает модели квалифицированные имена `project__*`. `list_files`, `search_files`, `read_file`, `git_diff` и `codegraph_explore` read-only; только `write_file` state-changing. JSON requests используют `deny_unknown_fields`. Пользовательские limit values только уменьшают глобальные пределы и не могут их расширить.

Начальные hard limits фиксируются константами и отражаются в документации: путь 1024 Unicode scalar values, один файл и write content 256 KiB, read response 64 KiB, 10 000 просмотренных файлов, 32 MiB прочитанных при одном поиске, 200 совпадений, snippet 400 characters, tool output/diff preview 64 KiB и 15 seconds для дочерних Git/CodeGraph процессов. Результаты содержат `truncated` и машинно-читаемую причину. Новые зависимости: `ignore = "0.4"` для gitignore-aware обхода и `similar = "2"` для единого line-oriented unified diff в preview и result.

Альтернатива с одним универсальным `filesystem` tool создаёт неоднозначную схему, затрудняет read-only annotations и расширяет blast radius. Regex search не нужен для двух целевых сценариев и добавляет риск дорогих выражений; literal search остаётся воспроизводимым.

### 4. Единая workspace policy перед любым filesystem access

`ProjectWorkspacePolicy` хранит канонический root. Входной путь должен быть относительным, непустым, без root/prefix/parent components и NUL. Для существующей цели проверяются `symlink_metadata`, regular-file status, canonical target under root, размер и UTF-8. Для create канонизируется уже существующий parent; parent обязан быть каталогом внутри root, поэтому project tool не создаёт директории.

Hard denylist применяется и к traversal, и к прямому path: `.git`, `target`, `.env` и `.env-*`/`.env.*`, `.fox-llm.json`, `.fox-embeddings.json`, `.fox-sessions.db*`, `.fox-scheduler.db*`, `.fox-support.db*`, `fox-metrics.log`, `mcp.log`, SQLite sidecars и credential-like names, перечисленные в одном тестируемом predicate. Ошибка denylist не сообщает, существует ли файл. `ignore::WalkBuilder` использует `.gitignore`, не следует symlink и сортирует окончательный список относительных путей перед ответом; явный denylist имеет приоритет над ignore settings.

Файловый текст помещается в tool result как данные, а системный prompt запрещает выполнять содержащиеся в нём инструкции. Ни server, ни prompt не получают environment variables или credentials.

### 5. Preview и write используют одну подготовленную семантику

Shared helper `prepare_write(root, request)` парсит schema, применяет policy, читает текущее состояние, проверяет mode/hash и строит `PreparedWrite`: нормализованный path, exact new bytes, old/new hashes, changed flag и unified diff. Интерактивный approval для `project__write_file` вызывает этот helper и показывает его результат вместо raw JSON. При ошибке helper возвращает отказ без prompt.

После ответа «да» MCP handler заново вызывает `prepare_write` по тем же аргументам. Это закрывает TOCTOU между preview и execution: изменившийся hash или появившийся create target даёт `conflict`, а не overwrite. `changed=false` возвращается без открытия файла на запись. Для `changed=true` handler создаёт уникальный temporary regular file в том же существующем parent, записывает и синхронизирует bytes, сохраняет permissions существующей цели при replace и делает rename в пределах одной filesystem. Temporary file удаляется при ошибке. Result возвращает path, mode, changed, old/new hashes, bytes_written и тот же bounded diff.

Для каждого нового tool call с иными аргументами preview и подтверждение выполняются снова. Подтверждение одного файла не авторизует следующий. Отказ преобразуется существующим agent loop в structured cancellation и не вызывает MCP handler.

Альтернатива с full overwrite без hash короче, но может уничтожить параллельное изменение. Patch syntax уменьшает payload, однако сложнее валидировать и делает provider errors менее предсказуемыми; full content с optimistic concurrency лучше для лимита 256 KiB.

### 6. Два уровня проверок и воспроизводимый Day 34 сценарий

Filesystem unit tests используют `tempfile` и покрывают path normalization, denylist, direct ignored-file access, symlink escape, binary/oversized input, stable list/search order, line numbers, truncation, read hash, create/replace/no-op/conflict, atomic failure cleanup и bounded diff. Loopback test проверяет точный tool list/annotations и shutdown без registry side effects.

Scripted provider tests создают временный workspace минимум с `src/api.rs`, `src/app.rs` и `README.md`:

1. **Поиск API.** При наличии `.codegraph/` provider сначала вызывает `codegraph_explore` по имени API, затем `read_file` для релевантных Rust-файлов и завершает ответ. Runtime отклоняет преждевременный `search_files` машинно-читаемой ошибкой. Тест проверяет CodeGraph-first порядок, отсутствие широкого поиска по документации, отсутствие write/approval и неизменные bytes всех файлов. Отдельный fallback-тест покрывает `search_files` и обязательное явное чтение совпадения, чтобы доступ прошёл обычную проверку инвариантов и учитывался в лимите tool calls.
2. **Обновление документации.** Provider читает Rust source и README, вызывает `write_file replace` с полученным hash, approval разрешает один exact call, затем запрашивает diff. Тест проверяет сохранённый Markdown, trace-derived changed path и unified diff. Второй scripted запуск над итоговым workspace не выполняет write либо получает `changed=false`; hash и bytes остаются прежними.

Provider-neutral regression повторяет минимальный tool round-trip для OpenAI, Claude и Ollama adapters. Отдельные тесты проверяют отказ approval, stale hash, отсутствие исследования и сохранение глобального инварианта.

Ручная инструкция в `reports/day34/README.md` использует реальные файлы репозитория:

```text
/project Найди все места, где используется ToolExecutor, и перечисли пути с номерами строк
/project Сравни список COMMANDS в src/config.rs с README.md и создай или обнови reports/day34/commands-audit.md
```

Повтор второй команды должен дать no-op либо `changed=false`. Отчёт фиксирует ожидаемый tool flow и команды `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test` и `openspec validate add-project-file-assistant --strict`, но не содержит credentials или содержимое локальных конфигураций.

## Risks / Trade-offs

- **[Большой full-content аргумент увеличивает tokens]** → лимит 256 KiB, диапазоны чтения и ориентация демонстрации на небольшие source/docs files.
- **[Модель может проигнорировать требование исследовать файлы]** → trace gate не выдаёт непроверенный текст как успешный project result; scripted regression фиксирует поведение.
- **[Prompt injection в коде или документации]** → project content маркируется как недоверенные данные, инструменты не предоставляют shell/network, invariants проверяются до approval, пользователь видит exact diff.
- **[TOCTOU между preview и записью]** → повторная policy/hash/existence validation непосредственно в handler; конфликт требует нового чтения и подтверждения.
- **[Частичный результат при нескольких файлах]** → каждый файл является отдельным подтверждённым действием, trace и итог явно перечисляют только состоявшиеся записи; транзакционность не обещается.
- **[Git diff может включать предыдущие локальные изменения]** → фактический вклад workflow берётся из `write_file` results, а `git_diff` помечается как состояние workspace и фильтруется по запрошенным допустимым путям.
- **[Дополнительный loopback server увеличивает startup complexity]** → lifecycle копирует проверенный `HelpMcpRuntime`; его сбой изолирован от обычного чата и help.

## Migration Plan

1. Добавить dependencies и filesystem primitives с unit tests.
2. Добавить project MCP server/runtime и loopback lifecycle tests, не подключая его к основному агенту.
3. Добавить project-субагента, trace validation и scripted provider tests.
4. Подключить `/project`, preview approval и shutdown в интерактивном приложении.
5. Добавить Day 34 report и выполнить полный verification matrix.

Persistent schema и пользовательская конфигурация не меняются. Rollback состоит в удалении команды, runtime и новых dependencies; созданные пользователем project files остаются обычными файлами и не требуют миграции.
