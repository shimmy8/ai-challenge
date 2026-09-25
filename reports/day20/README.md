# День 20 — Orchestration MCP

`fox-llm` маршрутизирует инструменты нескольких MCP-серверов по именам
`server__tool` и выполняет общий последовательный pipeline. Серверы разделены по
ответственности: `github`, `reporting`, `workspace`, `calendar`, `ai`,
`telegram`, `orchestrator`. Инструменты включены по умолчанию; в конфигурации
хранится только denylist.

## Демонстрационные флоу

Интерактивный отчёт использует три сервера:

```text
github__repository_metadata
  -> github__project_activity
  -> reporting__calculate_github_metrics
  -> reporting__render_github_report
  -> workspace__save_report
```

План валидируется до первого вызова, JSON-результаты передаются через `$ref`, а
запись файла подтверждается пользователем. В trace сохраняются ordinal,
server ID, native tool, duration и явный outcome.

Периодическая сводка не привязана к календарю на уровне scheduler:

```text
calendar__list_events
  -> ai__generate_text
  -> telegram__send_message
```

Orchestrator сохраняет schedule JSON, pipeline JSON, authorization hash, runs и
упорядоченные step traces. Доступны ссылки `run.id`, `run.trigger` и
`run.scheduled_at`. Повторное интерактивное подтверждение на запуске не нужно;
`failed` и `unknown` останавливают цепочку без вызова последующих шагов.

## Запуск

Каждый сервер запускается отдельным процессом:

```bash
cargo run -- --mcp-server github --addr 127.0.0.1:8101
cargo run -- --mcp-server reporting --addr 127.0.0.1:8102
cargo run -- --mcp-server workspace --addr 127.0.0.1:8103
cargo run -- --mcp-server calendar --addr 127.0.0.1:8104
cargo run -- --mcp-server ai --addr 127.0.0.1:8105
cargo run -- --mcp-server telegram --addr 127.0.0.1:8106
cargo run -- --mcp-server orchestrator --addr 127.0.0.1:8107
```

После регистрации URL через `/mcp` агент получает единый квалифицированный
каталог. Один недоступный сервер переводит только свои маршруты в degraded
состояние. stdout отдельных процессов MCP-серверов содержит JSON-метаданные
старта и вызовов с correlation ID, сервером, инструментом, outcome и duration,
но без аргументов, результатов и секретов. Интерактивный `cargo run` эти
диагностические записи не печатает.
