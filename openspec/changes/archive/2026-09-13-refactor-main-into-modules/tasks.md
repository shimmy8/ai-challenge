## 1. Базовая линия и общие модели

- [x] 1.1 Запустить существующий `cargo test`, зафиксировать исходный успешный результат и убедиться, что рефакторинг начинается без известных регрессий.
- [x] 1.2 Создать `src/model.rs`, перенести общие value-типы `Provider`, `Message`, `CompressionStrategy`, `BranchState` и `ApiAnswer` с неизменными derive/serde-атрибутами и проверить сборку командой `cargo check`.

## 2. Конфигурация и LLM-провайдеры

- [x] 2.1 Перенести конфигурацию приложения, конфигурации провайдеров, legacy-миграцию, режимы ответа, значения по умолчанию и функции путей в `src/config.rs`; перенести связанные тесты и проверить как минимум `cargo test config_round_trip` и `cargo test migrates_legacy_config`.
- [x] 2.2 Перенести HTTP-вызовы OpenAI/Claude, разбор ответов, получение/фильтрацию моделей и правила температуры в `src/providers.rs`; сохранить payload и ошибки, затем проверить как минимум `cargo test parses_openai_response`, `cargo test parses_claude_response`, `cargo test filters_out_openai_models_for_other_apis` и `cargo test normalizes_temperature_for_selected_model`.

## 3. Агенты и хранение сессий

- [x] 3.1 Перенести `AgentSettings`, `Agent`, `AgentPool` и логику стратегий контекста/ветвления в `src/agent.rs`, распределить тесты агента в локальный тестовый модуль и проверить `cargo test creates_independent_agents_in_pool`, `cargo test summary_keeps_recent_messages_and_original_archive` и `cargo test branching_restores_independent_histories_from_checkpoint`.
- [x] 3.2 Предоставить узкие внутрикрейтные методы или снимок состояния агента, необходимые оркестрации и хранилищу, и проверить через `cargo check`, что поля `Agent` не пришлось массово открывать как `pub(crate)`.
- [x] 3.3 Перенести `SessionStore`, `SavedSession`, `SessionSnapshot`, SQL-схему/миграции и TOON-кодирование в `src/sessions.rs`; перенести тесты хранилища и проверить `cargo test toon_history_round_trips_multiline_messages`, `cargo test sqlite_store_saves_and_loads_session` и `cargo test sqlite_persists_all_branches_checkpoint_and_active_branch`.

## 4. CLI, метрики и оркестрация

- [x] 4.1 Перенести структуру JSONL-метрик, обработку `--dump-metrics`, путь и запись лога в `src/metrics.rs`; перенести связанные тесты и проверить `cargo test parses_dump_metrics_flag` и `cargo test appends_json_metrics_log`.
- [x] 4.2 Перенести rustyline helper, команды, баннер/status-bar, интерактивные выборы и обработчики команд в `src/cli.rs`, сохранив русскоязычный вывод и список команд; перенести CLI-тесты и проверить `cargo test completes_slash_commands` и `cargo test validates_compression_command_and_strategies`.
- [x] 4.3 Перенести инициализацию и основной цикл без логических преобразований в `src/app.rs` как `pub(crate) async fn run() -> Result<()>`, затем сократить `src/main.rs` до деклараций модулей и вызова `app::run()`; проверить целостность связей командой `cargo check`.

## 5. Финальная проверка границ и поведения

- [x] 5.1 Проверить зависимости между модулями и сократить избыточную видимость до минимально необходимой `pub(crate)`; подтвердить отсутствие предупреждений командой `cargo clippy --all-targets --all-features -- -D warnings`.
- [x] 5.2 Запустить `cargo fmt --all -- --check`, `cargo test` и `cargo build`, убедившись, что весь существующий набор тестов проходит, бинарный target собирается, а новых зависимостей в `Cargo.toml`/`Cargo.lock` нет.
