## Context

См. `proposal.md` для мотивации. Сейчас `SessionStore::open` в `src/sessions.rs` содержит полный schema batch, последовательные legacy `ALTER TABLE` и helper `has_column`. В то же время `choose_model` в `src/cli.rs` соседствует с `fetch_models`, построением provider-specific запросов, разбором ответов и фильтрацией model IDs. Оба участка уже имеют регрессионное покрытие, но границы ответственности размыты.

Рефакторинг объединяет две независимые внутренние перестройки в одном change и одном демонстрационном PR. Они не зависят друг от друга на уровне кода, поэтому каждая часть должна оставаться отдельно проверяемой, а общий quality gate выполняется после обеих.

## Goals / Non-Goals

**Goals:**

- Сделать `SessionStore::open` короткой orchestration-функцией с явным migration boundary.
- Сосредоточить schema SQL, проверки колонок и legacy-миграции в одном внутреннем модуле.
- Оставить в CLI только progress output, интерактивный выбор и сохранение модели.
- Поместить сетевой model discovery contract рядом с остальными provider integrations.
- Сохранить SQLite-схему, данные, HTTP-контракты, фильтры и пользовательские сообщения.
- Подтвердить поведенческую нейтральность сфокусированными и полными тестами.

**Non-Goals:**

- Не вводить нумерованные миграции, schema version или новую DDL-транзакцию.
- Не переписывать SQL, CRUD, TOON-кодирование или путь базы.
- Не вводить общий provider trait и не перестраивать generation flow.
- Не менять поддерживаемые модели, defaults провайдеров, UI `FuzzySelect` или формат конфигурации.
- Не выполнять реальные сетевые запросы в тестах и не добавлять зависимости.

## Decisions

### 1. Два независимых модуля внутри одного change

SQLite-часть реализуется в `src/sessions/migrations.rs`, а model discovery — в `src/providers/models.rs`. Их объединяет цель уменьшить ответственность крупных модулей, но между новыми модулями не создаётся зависимость. Это позволяет получить содержательный демонстрационный PR и при этом проверять каждый перенос отдельно.

Альтернатива — оставить два OpenSpec change и два PR. Она дала бы меньшие diff, но не соответствует выбранному сценарию одного рефакторинга для демонстрации AI-ревью.

### 2. Внутренний migration API и механический перенос SQL

`sessions.rs` объявляет приватный `mod migrations;`. Модуль предоставляет одну точку входа `pub(super) fn apply(connection: &Connection) -> Result<()>`. Она выполняет существующий `execute_batch`, затем те же проверки и добавления колонок в прежнем порядке; `has_column` переезжает вместе с миграциями.

`SessionStore::open` сохраняет открытие connection, контекст ошибки пути и установку прав `0600` на Unix, затем вызывает `migrations::apply(&connection)` и конструирует store. Никакие SQL literals, defaults, constraints, foreign keys или failure semantics намеренно не меняются.

Альтернатива — одновременно нормализовать SQL и добавить transaction/schema version. Она увеличивает риск изменения старых баз и исключена из поведенчески нейтрального refactor.

### 3. Model discovery размещается в `providers::models`

`providers.rs` объявляет дочерний `models` и реэкспортирует на уровне crate только функции, нужные CLI и тестам. В `src/providers/models.rs` переезжают `fetch_models`, request construction, безопасные сетевые ошибки, parsing и capability filter. Модуль не зависит от `cli`.

`choose_model` продолжает печатать прогресс, определять default index, вызывать `FuzzySelect` и сохранять выбор. Такое разделение оставляет UI orchestration в CLI, а HTTP-контракт — в provider-слое.

Альтернатива — корневой `model_discovery.rs`. Она создала бы отдельную верхнеуровневую ответственность, хотя discovery является частью интеграции с провайдерами.

### 4. Provider contracts переносятся без изменений

OpenAI продолжает использовать Bearer token, Claude — `x-api-key`, `anthropic-version` и `limit=1000`, Ollama — keyless local URL, а `Ollama Remote` — нормализованный сохранённый endpoint. `parse_model_ids` сохраняет правила удаления пустых и несовместимых IDs, сортировки и дедупликации. Русские connection/empty-list errors остаются дословно прежними.

Credential values и полные response bodies не включаются в assertions или diagnostics. Новый error enum не вводится, чтобы не менять пользовательский текст.

### 5. Characterization-тесты предшествуют переносу

SQLite-тесты фиксируют текущую схему, defaults, повторное открытие и legacy-колонки без потери данных. Model discovery tests фиксируют URL, headers, query parameters, endpoint normalization, parsing/filtering и безопасные ошибки через mock HTTP, без реальных provider API.

После каждого переноса выполняются его сфокусированные тесты. Полные `fmt`, `clippy` и `cargo test` запускаются после объединения обеих частей.

## Risks / Trade-offs

- [Общий PR будет крупнее и сложнее для ревью] → сохранить две явные группы изменений и тестов, не смешивать их в одних функциях или коммитах.
- [При переносе изменится порядок DDL или будет пропущена колонка] → переносить блоки механически и проверять `PRAGMA table_info` и legacy fixtures.
- [Попытка улучшить миграции изменит поведение старых баз] → не добавлять schema version, transaction или SQL-нормализацию.
- [Перенос auth logic изменит headers] → сначала зафиксировать request-builder tests для всех провайдеров.
- [Изменится фильтрация OpenAI models] → сохранить predicate дословно и расширить table-driven cases.
- [Visibility новых функций расширится] → использовать `pub(crate)` только для CLI/test call sites, остальные helpers оставить private или `pub(super)`.
- [Возникнет цикл `cli` ↔ `providers`] → `providers::models` зависит только от config, HTTP/JSON типов и общих provider helpers.

## Migration Plan

1. Добавить characterization-тесты SQLite schema/legacy behavior и model discovery contracts.
2. Выделить `sessions::migrations`, не меняя SQL и порядок операций.
3. Выделить `providers::models`, не меняя HTTP-контракт, фильтры и ошибки.
4. Выполнить сфокусированные проверки каждой части и общий quality gate.
5. Rollback выполняется возвратом refactor-коммитов: формат базы, конфигурация и persisted model names не меняются.
