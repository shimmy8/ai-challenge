# telegram-message-delivery Specification

## Purpose

Предоставляет контролируемый MCP-выход для отправки готового текста в единственный заранее настроенный Telegram-чат без раскрытия реквизитов доставки.

## Requirements

### Requirement: Telegram server exposes a fixed-destination sink
Сервер `telegram` SHALL предоставлять изменяющий состояние инструмент `send_message`, принимающий только непустой текст в установленном пределе. Bot token и chat ID MUST поступать только из локальной конфигурации сервера и MUST NOT приниматься как аргументы инструмента.

#### Scenario: Message is delivered
- **WHEN** подтверждённый вызов содержит допустимый текст и Telegram принимает сообщение
- **THEN** инструмент возвращает статус `delivered` и безопасный идентификатор сообщения

#### Scenario: Model proposes another recipient
- **WHEN** аргументы содержат неизвестное поле получателя, chat ID или URL назначения
- **THEN** сервер отклоняет вызов и не отправляет сообщение

### Requirement: Telegram outcome is explicit and non-retriable when uncertain
Инструмент SHALL различать результаты `delivered`, `failed` и `unknown`. Потеря соединения после отправки, когда принятие сообщения нельзя установить, MUST возвращать `unknown` и MUST NOT автоматически повторяться исполнителем pipeline.

#### Scenario: Telegram rejects the request
- **WHEN** Telegram однозначно отклоняет сообщение
- **THEN** инструмент возвращает `failed` с безопасным описанием причины

#### Scenario: Delivery acknowledgement is lost
- **WHEN** ответ теряется после отправки запроса
- **THEN** инструмент возвращает `unknown`, а orchestrator сохраняет этот результат без автоматического повторения шага

### Requirement: Telegram secrets never leave the server boundary
Telegram-сервер MUST исключать token, chat ID и URL с token из tool result, ошибок и stdout-логов. Отсутствующая или неполная конфигурация SHALL приводить к безопасной ошибке до сетевого запроса.

#### Scenario: Telegram configuration is missing
- **WHEN** token или chat ID не настроен
- **THEN** инструмент не выполняет сетевой запрос и возвращает обобщённую ошибку настройки без частичных значений
