# scheduled-background-jobs Specification

## Purpose

Позволяет локальному MCP-серверу надёжно выполнять сохраняемые задания по времени без открытого интерактивного CLI и предоставлять агенту агрегированную историю запусков.

## Requirements

### Requirement: Calendar digest is a generic scheduled pipeline
Периодическая календарная сводка SHALL представляться обычным заданием orchestrator со стадиями чтения событий, изолированной AI-обработки и Telegram-доставки. Задание MUST NOT использовать специальный calendar runner или calendar-specific поля хранения.

#### Scenario: Daily calendar digest is scheduled
- **WHEN** пользователь подтверждает ежедневный pipeline `calendar__list_events` → `ai__generate_text` → `telegram__send_message`
- **THEN** orchestrator сохраняет и выполняет его по общим правилам scheduled pipeline

#### Scenario: Calendar source fails
- **WHEN** чтение CalDAV завершается ошибкой
- **THEN** AI и Telegram шаги не выполняются, а история содержит failed для источника и skipped для последующих шагов
