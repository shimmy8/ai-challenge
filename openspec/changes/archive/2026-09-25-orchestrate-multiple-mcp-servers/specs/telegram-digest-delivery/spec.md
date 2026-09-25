## ADDED Requirements

### Requirement: Calendar digest composes independent MCP capabilities
Календарная сводка SHALL получать события через `calendar__list_events`, формировать текст через `ai__generate_text` и доставлять его через `telegram__send_message` как последовательные шаги generic scheduled pipeline. Ни один из этих серверов MUST NOT получать прямой доступ к секретам другого сервера.

#### Scenario: Digest pipeline succeeds
- **WHEN** Calendar возвращает события, AI формирует непустой текст и Telegram подтверждает доставку
- **THEN** история запуска содержит три успешных шага с отдельными структурированными результатами

#### Scenario: AI generation fails
- **WHEN** AI-инструмент не может сформировать текст
- **THEN** Telegram-инструмент не вызывается, а запуск не представляется доставленным

### Requirement: Digest delivery uses the fixed Telegram destination
Шаг календарной сводки MUST передавать Telegram-серверу только готовый текст. Получатель SHALL определяться исключительно локальной конфигурацией Telegram-сервера и MUST NOT сохраняться в определении задания или промежуточном trace.

#### Scenario: Scheduled job is inspected
- **WHEN** пользователь просматривает определение или историю календарной сводки
- **THEN** данные содержат текстовый маршрут pipeline, но не token, chat ID или Telegram URL
