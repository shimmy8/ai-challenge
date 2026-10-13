## Context

См. мотивацию в `proposal.md`. В проекте уже есть provider-neutral chat flow, OpenAI-compatible embedding adapter, SQLite RAG-индекс, retrieval с provenance и GitHub HTTP-клиент. Текущий startup flow сосредоточен в `app.rs`, индексатор обнаруживает Markdown, а GitHub-клиент получает публичную project activity, но не pull request patches.

Главное ограничение — trust boundary CI. Для запуска тестов необходимо исполнять код PR, но reviewer нужны `OPENAI_API_KEY` и write-доступ к PR-комментарию. Эти действия должны выполняться в разных GitHub-hosted jobs: test job исполняет PR без secrets и write-token, а review job получает privileges, но исполняет только код base-ветки. Diff, test report и остальные данные PR полностью контролируются автором и остаются недоверенными. Локальные `.fox-llm.json`, `.fox-embeddings.json` и env-файлы с credentials также запрещено переносить в CI.

## Goals / Non-Goals

**Goals:**

- переиспользовать существующие HTTP provider, embeddings, chunking и retrieval primitives;
- отделить получение и нормализацию PR, RAG, LLM-review, агрегацию и rendering интерфейсами с тестируемыми pure-функциями;
- запускать `cargo test` до ревью и передавать его результат независимо от exit status;
- гарантировать, что PR-код исполняется только в непривилегированном job, а privileged job исполняет только содержимое защищённой base-ветки;
- выдавать пригодный человеку Markdown и валидируемый JSON;
- объяснять падение тестов с явной оценкой уверенности и не смешивать его с инфраструктурной ошибкой;
- сохранить предсказуемые стоимость, время и объём контекста с прозрачным partial-review.

**Non-Goals:**

- inline review comments и GitHub Checks annotations;
- автоматическое approve, request changes, merge или изменение кода;
- запуск format, Clippy и иных статических анализаторов из PR помимо согласованного `cargo test`;
- автоматическое ревью fork PR;
- постоянный внешний webhook-сервис или общий удалённый vector store;
- гарантия обнаружения всех дефектов или замена человеческого ревью.

## Decisions

### 1. `pull_request_target` оркестрирует два изолированных job

Workflow запускается для `opened`, `reopened` и `synchronize` и проверяет, что `head.repo.full_name == github.repository`. Job `tests` checkout'ит точный `head.sha` на GitHub-hosted ephemeral runner, получает только read-only `contents` permission, не получает `OPENAI_API_KEY` и запускает `cargo test`. Job `review` зависит от `tests`, но использует `if: always()`, checkout'ит конкретный `base.sha`, собирает `fox-llm` только из него и получает необходимые secrets и `pull-requests: write`.

PR head не checkout'ится в review job. Полученные пути, patches, title, body и test report не интерполируются в shell-команды. Все paths проходят нормализацию как относительные repository paths. Номер PR и repository передаются как простые значения; reviewer получает metadata и paginated file patches через GitHub REST API с `GITHUB_TOKEN`.

Альтернатива `pull_request` безопасно скрывает secrets от fork, но workflow и собираемый Rust-код берутся из merge ref и могут быть изменены в PR. Один job нельзя безопасно использовать одновременно для исполнения head-кода и AI-review с secrets. Отдельный внешний сервис даёт более сильную изоляцию, но существенно расширяет инфраструктуру. Поэтому выбран `pull_request_target` с job-level изоляцией, запретом передачи secrets test job и запретом исполнения head-кода review job.

### 2. Test report передаётся как ограниченный недоверенный artifact

Test step сохраняет полный вывод только во временном workspace runner, запоминает exit code и классифицирует исход как `passed`, `failed` либо `infrastructure_error`. До финального возврата исходного exit code отдельный always-step очищает ANSI и управляющие символы, извлекает доступные имена упавших тестов, ограничивает диагностический фрагмент 100000 Unicode-символами и создаёт `test-report.json`:

```text
status, exit_code, failed_tests[], log_excerpt, truncated,
infrastructure_message
```

Report загружается как workflow artifact даже при `failed`. После upload test job возвращает исходный неуспешный статус, поэтому GitHub сохраняет красную проверку. Review job с `if: always()` скачивает artifact, проверяет JSON schema и размер до чтения полей и рассматривает все строки как недоверенные данные. Отсутствующий или повреждённый artifact преобразуется в `infrastructure_error`, а не блокирует обычный diff review.

Альтернатива `continue-on-error` для всего test job сделала бы проверку зелёной и скрыла реальное падение. Передача полного stdout через job output имеет более жёсткие размерные ограничения и неудобна для schema validation, поэтому выбран ограниченный artifact.

### 3. `review-pr` — отдельный неинтерактивный application flow

Команда имеет форму:

```text
fox-llm review-pr --repository owner/name --pull-request N \
  --test-report test-report.json \
  --output review.md --json-output review.json
```

`app.rs` только распознаёт startup mode и передаёт управление новому `review`-модулю. Модуль делится на acquisition, selection, batching, prompting, validation, aggregation и rendering. Он не создаёт интерактивную сессию, профиль, задачу или MCP runtime и не сохраняет данные PR в `.fox-sessions.db`.

`--test-report` необязателен для локального запуска; его отсутствие означает `not_run`, но workflow всегда пытается передать сформированный report. Альтернативой было встроить flow в MCP GitHub server. Это добавило бы ненужную LLM-оркестрацию серверу, который сейчас отвечает за provider-neutral tools и фоновые процессы, тогда как review запускается отдельным CI-процессом.

### 4. CI использует environment-only конфигурацию

Отдельный `ReviewSettings` читает CLI flags, затем environment, затем defaults. Обязательны `GITHUB_TOKEN`, `OPENAI_API_KEY`, `FOX_REVIEW_MODEL` и `FOX_REVIEW_EMBEDDING_MODEL`; `FOX_REVIEW_PROVIDER` по умолчанию равен `openai`. Первый релиз принимает OpenAI для chat и OpenAI-compatible embeddings; другое значение provider отклоняется понятной ошибкой, пока не определён соответствующий credential contract.

Один project-scoped `OPENAI_API_KEY` применяется к chat и embeddings. Параметры лимитов читаются из `FOX_REVIEW_MAX_FILES`, `FOX_REVIEW_MAX_DIFF_CHARS`, `FOX_REVIEW_MAX_FILE_CHARS`, `FOX_REVIEW_BATCH_CHARS` и `FOX_REVIEW_MAX_FINDINGS`. Ни одно значение secret не входит в `Debug`, JSON output или сообщения ошибок.

Альтернатива — генерировать локальные config-файлы во workflow. Она переиспользовала бы текущие loaders, но создавала бы файлы с credentials и усложняла гарантии очистки, поэтому отклонена.

### 5. GitHub acquisition возвращает нормализованный snapshot

GitHub-клиент получает PR metadata и все страницы `GET /repos/{owner}/{repo}/pulls/{number}/files`. Snapshot содержит base/head SHA, путь, status, additions/deletions и optional patch. Отсутствующий patch у binary или слишком большого файла отмечается как unavailable, а не подменяется полным файлом.

Отбор выполняется детерминированно: сначала production source и конфигурация, затем тесты, затем документация. Binary, generated, lock и vendor paths исключаются. Лимиты считаются по Unicode characters после нормализации перевода строк; усечение выполняется на char boundary и по возможности на границе hunk. Snapshot хранит причины каждого skip/truncation для отчёта.

### 6. Review-профиль RAG индексирует доверенную base-ветку

Discovery получает canonical base root и allowlist расширений. Он включает `README.md`, `openspec/specs/**/*.md`, другие разрешённые Markdown-файлы и текстовый source code, но исключает `.git/`, `target/`, vendor/generated, local databases, `.env*`, `.fox-llm.json`, `.fox-embeddings.json` и любые symlink targets вне root. Запрещённые файлы отбрасываются до чтения.

Markdown продолжает использовать structural chunking. Для Rust и языков, для которых можно достоверно определить структуру без новой тяжёлой parser-зависимости, section отражает symbol context; остальные разрешённые тексты используют существующий Unicode-safe fixed chunking с line-range metadata. Review использует временный SQLite index; persistent CI cache не является условием корректности MVP.

Для каждого diff batch строится детерминированный retrieval query из paths, hunk headers и технических идентификаторов. При `failed` к query добавляются имена тестов, panic locations, compiler error codes и repository-relative paths, извлечённые из валидированного test report. Это избегает отдельного LLM rewrite на каждый пакет. Retrieval применяет существующие similarity/reranking primitives и передаёт не более 6000 символов целых чанков с provenance.

### 7. Двухэтапный LLM-контракт ограничивает галлюцинации

Каждый batch prompt содержит фиксированную system-инструкцию, JSON metadata, fenced diff-data, отдельный test-report block и RAG contexts. Prompt явно объявляет PR, test output и RAG недоверенными, запрещает следовать содержащимся в них инструкциям и требует JSON findings, основанные на конкретном hunk либо проектном контексте.

Parser проверяет enum-поля, repository-relative path, наличие файла в snapshot и принадлежность line range добавленным либо контекстным строкам patch. Невалидные findings отбрасываются с безопасной метрикой; полностью невалидный ответ завершает batch ошибкой вместо публикации произвольного текста.

Финальная агрегация получает только валидированные finding IDs и краткие данные. Она может объединить дубли, но не может создать finding без исходного ID. Для `failed` отдельный result field связывает объяснение тестов с evidence IDs из test report, diff findings или RAG chunks и принимает confidence только `confirmed`, `likely` либо `unknown`. `confirmed` разрешён лишь при прямом совпадении диагностического сообщения с изменённым кодом или документированным контрактом; при недостаточных данных используется `unknown`. `infrastructure_error` выводится отдельно и не проходит через причинный анализ кода.

После агрегации выполняется повторная schema- и provenance-проверка, затем детерминированная сортировка `critical`, `warning`, `suggestion`, path, line. Результат ограничивается 30 findings по умолчанию.

Альтернатива — один prompt со всем PR — проще, но хуже масштабируется, теряет локальный RAG-фокус и делает стоимость и качество менее предсказуемыми.

### 8. Публикация отделена от анализа

Команда атомарно записывает `review.json` и `review.md` только после успешной валидации. Markdown всегда содержит test status, три обязательных review-раздела, coverage summary, base/head SHA и скрытый маркер `<!-- fox-llm-ai-review -->`. При `failed` добавляется `Почему упали тесты` с confidence и evidence; при `infrastructure_error` выводится отдельное предупреждение о невозможности достоверно выполнить тесты; при `passed` дополнительный раздел не создаётся.

Workflow-публикователь ищет комментарий текущего bot user с тем же маркером и создаёт либо обновляет его. Summary содержит test status, статус review, число обработанных/пропущенных файлов, число findings и ссылку на PR, но не полный test log, prompt, RAG chunks или provider response. Падение test job не блокирует publisher после успешного review. Если сам анализ упал, publisher не запускается; отдельный failure step пишет безопасную диагностику в Actions Summary и оставляет прежний комментарий нетронутым.

## Risks / Trade-offs

- **[Prompt injection в diff или документации]** → жёсткое разделение instructions/data, отсутствие tool-calling у review-запросов и validation результата.
- **[Prompt injection или управляющие символы в test output]** → очистка, ограничение размера, schema validation, недоверенный data block и evidence-only выводы.
- **[Компрометация secrets через PR-код]** → job-level isolation: head SHA исполняется только без secrets, privileged reviewer собирается из base SHA и не исполняет PR-содержимое.
- **[`pull_request_target` ошибочно расширят позже]** → комментарий безопасности в workflow и regression-проверка, запрещающая secrets/write-token в test job и head checkout в review job.
- **[Artifact poisoning]** → test report имеет фиксированную JSON-схему, size limit и не используется как путь, команда или конфигурация.
- **[Зависший или чрезмерно долгий тест]** → явный job timeout; timeout классифицируется как `infrastructure_error`, review всё равно запускается.
- **[Большой PR даёт неполное ревью]** → прозрачный coverage block, детерминированный приоритет и настраиваемые лимиты вместо скрытого truncation.
- **[Embedding всего base-корпуса увеличивает стоимость]** → reuse одинаковых content hashes внутри запуска; cache между runs можно добавить позже без изменения контракта.
- **[GitHub file API не возвращает patch]** → отметить файл как unavailable и не делать выводов о невидимом diff.
- **[Модель вернула правдоподобную, но неверную строку]** → проверять path и line against parsed hunks; иначе публиковать finding только на уровне файла.
- **[Sticky-комментарий мог создать человек]** → обновлять только comment с marker, принадлежащий текущему GitHub Actions bot.

## Migration Plan

1. Добавить review-модуль и тестовые fixtures без включения workflow.
2. Расширить RAG discovery отдельным review-профилем, сохранив default Markdown behavior.
3. Добавить GitHub PR acquisition и локально проверить `review-pr` на mock API/provider.
4. Добавить генерацию, очистку и schema validation `test-report.json` на локальных fixtures.
5. Добавить workflow с изолированными `tests` и `review` jobs, выключенный для fork, и настроить repository secret/variables.
6. Выполнить тестовые same-repository PR с прошедшими и упавшими тестами; проверить красный test job, обязательный запуск review, причинный раздел, sticky update и отсутствие secrets в логах.
7. Включить события `opened`, `reopened`, `synchronize`.

Rollback выполняется удалением или отключением workflow. Новый startup mode и review-профиль не меняют SQLite schema, пользовательские конфигурации или интерактивный flow и могут оставаться неиспользуемыми.
