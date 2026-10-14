## Context

См. мотивацию в `proposal.md`. Сейчас `review-pr` является startup-mode монолитного binary: `src/main.rs` объявляет все модули приложения, а `.github/workflows/ai-review.yml` выполняет `cargo build --release`. Review-модуль до generation синхронно строит embedding-индекс всего разрешённого корпуса; `error_for_status()` превращает первый `429` в фатальную ошибку. Отдельный `fork-notice` существует только для выполнения прежнего требования об Actions Summary для fork.

Review job работает на trusted `base.sha`; эти границы доверия и минимальные permissions должны сохраниться. Реальные credentials из локальных env/config файлов не читаются ни при разработке, ни в CI.

## Goals / Non-Goals

**Goals:**

- завершать полезное и проверяемое review при временной недоступности embeddings;
- переиспользовать embeddings неизменившихся чанков между trusted CI-запусками и отправлять provider только cache misses;
- не скрывать degraded retrieval и не маскировать постоянные ошибки конфигурации;
- объяснять пользователю, что изменил PR, и почему semantic retrieval деградировал;
- собирать в привилегированном job отдельный reviewer с минимальным Rust module/dependency graph;
- ускорять PR-тесты без права недоверенного head-кода записывать общий build cache;
- убрать runner, не выполняющий ничего кроме fork notice, сохранив запрет provider calls для fork;
- оставить `fox-llm review-pr` совместимым для локальных вызовов и существующей документации.

**Non-Goals:**

- fallback для недоступности chat-модели;
- внешний storage service или cache, общий между репозиториями;
- смена provider, модели, лимитов diff или формата findings;
- запуск AI-review для fork PR;

## Decisions

### 1. Выделить `fox-review` в минимальный workspace crate

Review-код переносится в crate `crates/fox-review` с библиотечным API и одноимённым binary. Crate включает только CLI parsing, GitHub acquisition, test-report parsing, corpus discovery/retrieval, review-specific OpenAI client, validation и rendering. Корневой `fox-llm` зависит от этой библиотеки и сохраняет startup-mode `review-pr` как тонкий совместимый адаптер.

Workflow собирает `cargo build --release --package fox-review --bin fox-review` и запускает `target/release/fox-review` без промежуточного `review-pr`. Корневой Cargo manifest объявляет workspace, а review-crate перечисляет только реально используемые зависимости. Это даёт Cargo отдельный package graph; один лишь `--bin` внутри текущего package не гарантировал бы исключение всех обычных зависимостей монолита.

Альтернативы:

- `cargo build --release --bin fox-llm` сохраняет текущую лишнюю сборку и отклонён;
- второй binary в корневом package проще по структуре, но потребовал бы сложной optional-feature матрицы и продолжал бы связывать review с монолитным manifest;
- копирование review-кода в отдельный script создаёт две реализации контракта и отклонено.

### 2. Дать review-crate узкий provider adapter

Review-specific adapter напрямую реализует необходимые OpenAI Responses и Embeddings запросы и возвращает типизированные безопасные ошибки. Он не зависит от `Agent`, memory, MCP, interactive config или session types. Общий `fox-llm` делегирует review библиотеке, поэтому бизнес-логика и сетевое поведение не дублируются между двумя entry points.

Тип embedding-ошибки различает:

- retryable: `429`, `5xx`, timeout и временный transport error;
- permanent: `400`/`401`/`403` и другие ошибки запроса или доступа;
- invalid response: успешный HTTP-ответ с несовместимым JSON, числом или размерностью vectors.

Тексты ошибок и metadata не содержат API key или response body. Permanent и invalid-response ошибки остаются фатальными, чтобы неверная конфигурация не выглядела как штатная деградация.

### 3. Content-addressed SQLite cache восстанавливается только trusted job

Review-crate получает необязательный путь cache через явный CLI argument или environment variable CI. SQLite cache хранит metadata namespace и таблицу vectors, индексированную по content hash. В cache не записываются API credentials, PR metadata, исходный текст chunk или test report.

Namespace внутри SQLite включает cache schema version, provider endpoint origin, embedding model, dimensions и chunking/normalization version. Перед использованием corpus vectors reviewer получает первый live query vector: его размерность подтверждает runtime dimensions выбранной модели. При несовпадении любого поля cache целиком считается несовместимым. Текущий корпус заново обнаруживается из trusted `base.sha`; cache лишь подставляет vectors по content hash, поэтому удалённый или переименованный source не возвращается в retrieval из устаревшей строки. Перед сохранением строки, чьи content hashes отсутствуют в текущем корпусе, удаляются, ограничивая database объёмом актуального корпуса.

Workflow восстанавливает cache в `.ai-review-cache/review-rag.db` только внутри review job после checkout `base.sha`. Actions cache key состоит из версии формата, runner OS, base branch и безопасного hash известных до запуска полей — provider origin, model и chunking version; runtime dimensions дополнительно проверяются по SQLite metadata и первому live query vector. Уникальный suffix запуска позволяет сохранить обновлённый immutable Actions cache, а стабильный restore prefix выбирает последний совместимый snapshot. Test job не содержит restore/save steps и не получает возможность публиковать значения в этот namespace.

Cache всегда считается недоверенным: перед чтением выполняются SQLite integrity/schema checks; каждый vector проверяется на ожидаемую размерность и finite values. Ошибка чтения или несовместимость превращается в cache miss, а не в падение review. После обработки валидные новые vectors и pruning актуального корпуса фиксируются транзакционно, database checkpoint/close завершается до cache save. Workflow сравнивает SHA-256 закрытого database до и после reviewer и запускает save step для существующего изменившегося файла независимо от исхода последующей generation или публикации. Так проверенные batches не теряются при ошибке после retrieval; публикация по-прежнему запускается только для полностью сформированного review. В Actions Summary выводятся только counts reused/new без cache contents.

Альтернативы:

- ключ по `base.sha` без restore prefix отклонён, поскольку почти каждый merge обнулял бы reuse;
- artifact между workflow runs отклонён как неудобный для incremental restore;
- внешний object store или database отклонён как лишняя инфраструктура и новый credential boundary.

### 4. Тайм-аут запроса отделён от бюджета индексации

HTTP-клиент embeddings ограничивает одну попытку 30 секундами, а orchestration
даёт холодному построению индекса общий бюджет 5 минут. После каждого полностью
проверенного batch текущая content-addressed карта атомарно сохраняется в
SQLite до следующего await. Поэтому отмена общего future оставляет пригодный
частичный cache, который trusted workflow сохраняет и использует при повторе.
Прогресс counters хранится отдельно от отменяемого future и попадает в fallback
результат. Ошибка записи cache становится фатальной до следующего embedding
request: иначе reviewer не мог бы гарантировать сохранение уже готового batch.

### 5. Retry ограничен, затем включается circuit-breaker на один запуск

Каждая embedding-операция получает не более трёх попыток в пределах существующего общего embedding timeout. Для `429` используется валидный `Retry-After`, ограниченный 10 секундами; без него и для остальных retryable ошибок применяется exponential backoff с jitter и верхней границей 10 секунд.

Если retryable ошибка исчерпала попытки при построении индекса или query embedding, текущий embedding-индекс отбрасывается, режим запуска переключается на `lexical_fallback`, и до конца процесса новые embedding-запросы не выполняются. Это предотвращает каскад `429` на каждом batch и упрощает provenance: один запуск использует один объявленный retrieval mode.

Альтернатива «только retry» отклонена: quota exhaustion может пережить timeout job. Безусловный fallback на любую ошибку отклонён, потому что скрывал бы сломанный API key или неверную модель.

Cache hits загружаются до сетевых запросов, а batching охватывает только misses. Если retryable ошибка исчерпала попытки, все уже проверенные vectors MAY остаться в cache для следующих запусков, но текущий review целиком переключается на lexical retrieval, чтобы результат имел один однозначный retrieval mode.

### 6. Lexical retrieval использует тот же корпус и бюджет

Fallback не создаёт SQLite vector index. Он нормализует query и поля chunk в Unicode-aware lowercase tokens, учитывает совпадения source path и section выше совпадений content, затем сортирует по score и стабильному chunk id. В контекст попадают только чанки с ненулевым score, целиком и в существующем лимите 6000 Unicode-символов.

`ReviewChunk` остаётся единственным источником provenance. Поэтому существующая проверка evidence IDs, защита от prompt injection и запрет выдуманных sources применяются одинаково к semantic и lexical контексту.

### 7. Режим retrieval является частью результата

`ReviewResult` получает обязательное enum-поле `retrieval_mode`: `embedding` или `lexical_fallback`, необязательную типизированную причину fallback (`rate_limited`, `provider_unavailable`, `timeout`, `transport_error`), а также безопасные counters `cached_embeddings` и `new_embeddings`. Markdown добавляет предупреждение только для fallback и объясняет его безопасной категорией, не сырым provider message. Publisher читает подготовленный review JSON и записывает режим и cache counters рядом с test status в Actions Summary. Сырой provider error и cache contents в пользовательские артефакты не попадают.

Модель возвращает для каждого batch краткие элементы `change_summary` с обязательными `diff:<path>` evidence IDs. Validator принимает только непустой текст с evidence из реально переданного diff, aggregation дедуплицирует элементы, а Markdown выводит их в отдельном разделе `Что изменилось` перед findings. Это отличает описание состава PR от замечаний о качестве.

### 8. Русский язык задаётся trusted prompt

Trusted prompt явно требует русский язык для `change_summary.summary`,
`findings.title`, `findings.explanation`, `findings.recommendation` и
`test_failure_explanation.summary`. После JSON parsing reviewer проверяет schema,
evidence и остальные структурные инварианты, но не классифицирует язык
эвристически и не повторяет generation только из-за английских слов. Это
устраняет ложные отказы на технических терминах и второй дорогой LLM-вызов.
Технические пути, идентификаторы, enum и evidence IDs не переводятся.

### 9. Fork PR пропускается только guards

В `tests` и `review` сохраняется same-repository condition. `fork-notice` удаляется; для fork оба содержательных job отображаются GitHub как skipped, workflow не получает secrets и не занимает runner ради поясняющего echo. Regression test проверяет отсутствие `fork-notice`, наличие guards и отсутствие provider credentials в test job.

### 10. Generation payload учитывает контракт reasoning-моделей

Review-specific adapter сохраняет `temperature: 0`, а для семейства `gpt-5.1`–`gpt-5.6` дополнительно передаёт `reasoning.effort: none`, как рабочий adapter основного приложения. Формирование payload выделяется в чистую функцию и покрывается регрессионными тестами для reasoning- и обычной модели.

При неуспешном Responses API ответе adapter выводит HTTP status и только безопасные структурированные поля `error.type`, `error.code` и `error.param`. Полный body и `error.message` не попадают в диагностику, поскольку могут содержать отправленные данные или чувствительные детали.

### 11. Cargo build cache отделён от RAG cache

Trusted review job восстанавливает через официальный `actions/cache@v4` Cargo registry, git checkout cache и `target/release`. Ключ включает runner OS и `Cargo.lock`; Cargo fingerprints дополнительно защищают от использования несовместимых артефактов после обновления toolchain. Поскольку `pull_request_target` теперь получает read-only cache token по умолчанию, только review job явно задаёт `cache-mode: write`. Это допустимо, потому что он checkout-ит и исполняет только доверенный `base.sha`; test job такого режима не получает.

RAG cache остаётся отдельным namespace и сохраняется, если trusted reviewer изменил существующий SQLite-файл, даже когда последующая generation или публикация завершилась ошибкой. Ошибка generation также не мешает GitHub автоматически сохранить уже прогретый Cargo cache, поэтому повторный запуск переиспользует оба вида выполненного прогрева.

### 12. Test build cache прогревается только из main

Отдельный workflow на `push` в `main` и `workflow_dispatch` выполняет `cargo test --workspace --no-run` и сохраняет Cargo registry/git и `target/debug` с ключом по runner OS, точной версии Rust и `Cargo.lock`. Он работает только с доверенным main и имеет `cache-mode: write`.

PR test job остаётся на `pull_request_target`, явно использует `cache-mode: read` и только восстанавливает совместимый test cache перед `cargo test`. Он не содержит save action, поэтому build scripts и код `head.sha` не могут отравить cache; новые PR-артефакты компилируются локально и исчезают вместе с runner.

## Risks / Trade-offs

- [Lexical retrieval менее точен, чем embeddings] → явно маркировать fallback, сохранять строгий контекстный бюджет и отдавать повышенный вес path/symbol tokens.
- [Workspace extraction затронет visibility и тестовые пути] → сначала перенести review как библиотеку без изменения поведения, сохранить unit-тесты рядом с кодом и только затем переключить workflow.
- [Backoff увеличит latency перед fallback] → ограничить три попытки, отдельную задержку и общий timeout; после первого исчерпания открыть circuit-breaker.
- [Actions cache может отсутствовать, быть вытеснен или содержать повреждённый SQLite] → считать cache необязательной оптимизацией, валидировать перед чтением и сохранять обычный embedding path с lexical fallback.
- [Параллельные PR сохраняют immutable cache snapshots с одним restore prefix] → использовать уникальный save key на запуск; любой snapshot остаётся content-addressed и корректным, а последующий запуск дополняет наиболее свежий доступный.
- [Cache poisoning из недоверенного head-кода] → выполнять restore/save исключительно в review job на trusted `base.sha`, не передавать cache steps или namespace test job.
- [GitHub Summary parser может разойтись с renderer] → передавать publisher машинный `review.json` и проверять enum schema вместо свободного текста.
- [Удаление fork notice уменьшает объяснимость] → skipped state двух jobs остаётся видимым в Actions; README явно документирует fork policy.
- [Скомпрометированный build cache мог бы влиять на privileged binary] → cache создаётся и восстанавливается только job, который checkout-ит доверенный `base.sha`; ключ привязан к `Cargo.lock`, а PR test job не использует этот namespace.
- [Provider меняет допустимые параметры Responses API] → держать payload builder изолированным, тестировать семейства моделей и сохранять структурированную безопасную диагностику для следующего уточнения контракта.
- [Write-capable cache на low-trust trigger создаёт риск poisoning] → разрешить запись только review job, который исполняет `base.sha`; test cache публиковать отдельным trusted main workflow, а PR test job оставить read-only.
- [Описание изменений может галлюцинировать] → требовать известные `diff:` evidence IDs, ограничивать длину и отбрасывать элементы без подтверждённого diff.
- [Модель может нарушить языковое указание prompt] → принять это как ограничение prompt-only подхода; не срывать структурно корректное review неточным эвристическим классификатором.

## Migration Plan

1. Создать минимальный review crate и перевести корневой `review-pr` на его публичный API при сохранении всех текущих тестов.
2. Добавить content-addressed cache store и incremental indexing, затем типизированные embedding errors, bounded retry, lexical retriever и `retrieval_mode` с regression tests.
3. Переключить workflow на сборку и запуск `fox-review`, добавить trusted restore/save cache steps, передать `review.json` publisher и удалить `fork-notice`.
4. Обновить workflow tests, README и отчёт дня 32; прогнать форматирование, Clippy, unit/integration tests и строгую OpenSpec validation.
5. Добавить совместимый generation payload, безопасную диагностику, typed fallback reason, change summary и trusted Cargo build caches для reviewer и тестов.
6. Проверить последовательные same-repository PR runs с cache reuse, доступными embeddings и контролируемым `429`; убедиться в снижении новых embedding requests, ускорении повторной сборки/тестов и успешной публикации понятного degraded review без повторного sticky-комментария.

Rollback: вернуть workflow к `fox-llm review-pr` и предыдущему commit. Выделенный crate не меняет данные или схемы, поэтому миграция состояния не требуется.
