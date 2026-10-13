## 1. Зафиксировать текущее поведение

- [x] 1.1 Добавить characterization-тест `sqlite_current_schema_is_complete_and_reopen_is_idempotent`, который на временной базе проверяет обязательные таблицы, колонки, defaults и повторное `SessionStore::open`; проверить командой `cargo test sqlite_current_schema_is_complete_and_reopen_is_idempotent`.
- [x] 1.2 Добавить table-driven regression test для legacy-схем без мигрируемых колонок `tasks`, `invariants`, `sessions` и `session_context`; проверить сохранение старых строк и новые defaults командой `cargo test sqlite_legacy_columns_are_added_without_data_loss`.
- [x] 1.3 Расширить request-builder tests таблицей OpenAI, Claude, Ollama и `Ollama Remote`: проверить URL, `limit=1000`, `anthropic-version`, нужный способ авторизации и отсутствие `Authorization` у Ollama; выполнить сфокусированный model discovery request test.
- [x] 1.4 Расширить table-driven tests разбора и фильтрации для всех провайдеров, пустых/повторяющихся IDs, fine-tuned OpenAI, specialized/legacy моделей и malformed JSON; проверить сортировку и дедупликацию сфокусированным test filter.
- [x] 1.5 Добавить mock HTTP-тест ошибок discovery, проверяющий прежние русские сообщения и отсутствие API key/response body в диагностике; выполнить этот тест отдельно.

## 2. Выделить SQLite migration boundary

- [x] 2.1 Создать `src/sessions/migrations.rs` с `pub(super) fn apply(&Connection) -> Result<()>`, механически перенести текущий schema batch, порядок `ALTER TABLE` и `has_column`; проверить SQLite-тесты из группы 1.
- [x] 2.2 Объявить приватный `sessions::migrations`, сократить `SessionStore::open` до открытия файла, Unix permissions, вызова `migrations::apply` и создания store; проверить существующие legacy/reopen тесты.
- [x] 2.3 Проверить, что SQLite-refactor не добавил schema version, DDL transaction и изменений SQL/defaults, сопоставив diff с `design.md` и выполнив `git diff --check`.

## 3. Выделить model discovery boundary

- [x] 3.1 Создать `src/providers/models.rs`, перенести `fetch_models`, request construction, response parsing, compatibility predicate и provider-specific ошибки без изменения логики; проверить model discovery tests из группы 1.
- [x] 3.2 Объявить подмодуль в `providers.rs` и сделать минимальные crate-level re-exports для CLI/тестов, не создавая зависимости от `cli`; проверить командой `cargo check --all-targets`.
- [x] 3.3 Оставить в `cli.rs` только `choose_model` и интерактивный выбор, обновить imports/call sites и удалить перенесённый код; проверить существующими CLI-тестами неизменность progress/prompt и сохранения выбранной модели.
- [x] 3.4 Проверить неизменность URL, headers, filters и error strings относительно characterization-тестов и выполнить `git diff --check`.

## 4. Полная проверка объединённого рефакторинга

- [x] 4.1 Выполнить `cargo fmt --all -- --check` и `cargo clippy --all-targets --all-features -- -D warnings`; исправить все замечания.
- [x] 4.2 Выполнить полный `cargo test` и подтвердить успешное прохождение новых, SQLite legacy, model discovery и остальных регрессионных тестов.
- [x] 4.3 Просмотреть итоговый diff как единый демонстрационный PR и подтвердить отсутствие изменений публичного поведения, конфигурации, схемы данных и зависимостей.
