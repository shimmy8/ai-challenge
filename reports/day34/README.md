# День 34. Ассистент для работы с файлами проекта

`/project <цель>` выполняет реальную работу с файлами текущего репозитория:
самостоятельно ищет релевантные места, читает их, анализирует результат и при
необходимости предлагает ограниченный diff. Каждый запуск использует нового
изолированного агента и отдельный project-only MCP runtime.

## Архитектура

```text
цель пользователя
      |
      v
изолированный Agent ---- scripted/OpenAI/Claude/Ollama adapter
      |
      v
ProjectWorkflowExecutor ---- evidence текущего запуска
      |
      v
loopback project-only MCP ---- ProjectWorkspacePolicy ---- файлы репозитория
```

Runtime публикует ровно шесть qualified tools:
`project__list_files`, `project__search_files`, `project__read_file`,
`project__write_file`, `project__git_diff` и `project__codegraph_explore`.
Shell, сеть, пользовательские MCP-серверы, память, RAG и история родительского
диалога этому агенту не передаются.

## Границы безопасности

- принимаются только относительные пути внутри canonical workspace;
- symlink, binary, слишком большие файлы, `.git`, `target`, env-файлы,
  credentials, локальные базы и логи отклоняются;
- обход проекта учитывает `.gitignore`, но явно указанный безопасный ignored
  файл можно прочитать;
- `replace` требует SHA-256 именно той версии, которая прочитана в текущем
  workflow; `create` требует точной проверки отсутствия пути;
- перед записью пользователь видит hashes, размер и unified diff;
- после подтверждения precondition проверяется повторно; обнаруженное на этой
  проверке конкурентное изменение даёт conflict и не перезаписывается;
- ответы поиска, чтения, diff и дочерних процессов имеют фиксированные лимиты.
- запрос локального env-файла или другого denylisted пути завершается
  детерминированным безопасным отказом без подтверждения существования,
  повторения пути или вывода содержимого; workspace остаётся неизменным.

## Сценарий 1: найти использования API

Цель: «Найди все места, где используется `ToolExecutor`, и перечисли файлы».
Scripted проверка создаёт `src/api.rs`, `src/app.rs` и `README.md`. При наличии
индекса агент сначала вызывает `codegraph_explore`, затем читает релевантные
Rust-файлы и формирует итог из trace без широкого поиска по документации.
`search_files` используется как fallback, если CodeGraph недоступен. В сводке
найденные пути отделены от фактически прочитанных: только последние помечаются
как «Проверено». Approval ни разу не вызывается, bytes всех трёх файлов остаются
прежними.

Проверка:

```bash
cargo test symbol_lookup_is_codegraph_first_and_reads_relevant_sources
cargo test search_fallback_requires_an_explicit_file_read
cargo test failed_codegraph_allows_bounded_search_fallback
cargo test denied_secret_read_returns_safe_refusal_without_path_disclosure
cargo test loopback_mcp_denied_secret_read_returns_safe_refusal
```

## Сценарий 2: обновить документацию

Цель: «Обнови README по изменениям в коде». Агент сначала читает исходный код и
README, затем предлагает единственный `replace` с ожидаемым SHA-256. После
подтверждения результат содержит фактически изменённый путь и bounded unified
diff. Повторный запуск читает уже обновлённое состояние и не пишет файл.

Проверка:

```bash
cargo test documentation_update_is_hash_bound_and_repeatable
cargo test rejected_write_never_reaches_handler_and_is_reported
cargo test concurrent_edit_after_approval_is_not_overwritten
```

Пример отдельного создаваемого артефакта —
[`commands-audit.md`](commands-audit.md). При ручной демонстрации его можно
пересоздать целью «Сформируй `reports/day34/commands-audit.md` по командам из
README и исходного кода», проверить diff, подтвердить запись и повторить цель.
На втором запуске ожидается no-op с тем же hash.

## Полная проверка

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
openspec validate add-project-file-assistant --strict
```

Loopback-тесты требуют разрешения открывать локальный TCP-порт. Все сценарии
используют временные каталоги и scripted provider adapters, поэтому live ключи,
env-файлы, локальные базы и содержимое личных диалогов не нужны и не читаются.

Финальный прогон 16 октября 2026 года: форматирование и Clippy завершились без
замечаний, `cargo test` — 314 passed, 0 failed, strict OpenSpec validation —
успешно.
