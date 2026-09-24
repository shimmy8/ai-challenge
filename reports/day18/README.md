# День 18 — фоновые календарные сводки в Telegram

MCP-сервер теперь может сохранять расписание календарной сводки в отдельной
`.fox-scheduler.db`. Worker запускается внутри постоянно работающего процесса
`cargo run -- --mcp-server`, поэтому интерактивный CLI может быть закрыт.

## Архитектура

```text
fox-llm (MCP client) -> MCP control plane -> Scheduler worker
                                                |
                              CalDAV -> LLM -> Telegram
                                                |
                                      scheduler_runs (SQLite)
```

Доступные инструменты:

- `create_calendar_digest_schedule`
- `list_calendar_digest_schedules`
- `pause_calendar_digest_schedule`
- `resume_calendar_digest_schedule`
- `delete_calendar_digest_schedule`
- `run_calendar_digest_now`
- `get_calendar_digest_history`

Создание, изменение, удаление и ручной запуск требуют подтверждения в
интерактивном агенте. Telegram token и chat ID берутся только из `.env-mcp` и
не являются аргументами MCP-инструментов.

## Быстрый запуск

```bash
cp .env-mcp.example .env-mcp
# заполнить CalDAV и Telegram значения локально
cargo run -- --mcp-server
```

В другом терминале настройте MCP URL в `fox-llm`, включите новые инструменты
через `/mcp`, затем попросите агента создать ежедневную сводку, например:

> Каждый день в 08:00 присылай в Telegram сводку календаря на ближайшие 24 часа.

Для формулировки «на завтра» агент должен передать расписанию
`target_day: "tomorrow"`. При успешном ответе в Telegram отправляется только
сводка LLM; детерминированный список используется как резервный ответ при
ошибке или пустом ответе модели.

Для проверки без ожидания расписания используйте `run_calendar_digest_now`, а
затем `get_calendar_digest_history`. История показывает состояние `delivered`,
`failed`, `interrupted` или `unknown`, а также последнюю сводку.

Если LLM недоступна или вернула пустой ответ, запуск получает статус `failed` и
сообщение не отправляется. Если Telegram вернул неопределённый сетевой результат, запуск получает статус
`unknown` и автоматически не повторяется, чтобы не создать дубликат.

## Проверки

```bash
cargo fmt --all -- --check
cargo clippy --offline --all-targets --all-features -- -D warnings
cargo test --offline
openspec validate add-scheduled-telegram-digests --strict
```

Полный smoke-тест с реальными CalDAV и Telegram реквизитами выполняется только
локально. Секреты, Telegram-сообщения и `.fox-scheduler.db` в отчёт не входят.
