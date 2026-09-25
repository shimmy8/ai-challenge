## MODIFIED Requirements

### Requirement: Calendar events can be read for an absolute interval
Сервер `calendar` SHALL предоставлять read-only MCP-инструмент `list_events`, который принимает либо абсолютный полуоткрытый интервал `[from, to)`, либо ограниченное относительное окно с абсолютным `relative_to`, явным типом периода и временной зоной. Инструмент MUST преобразовать относительное окно в абсолютные границы до CalDAV-запроса и вернуть нормализованные события, упорядоченные по началу.

#### Scenario: Tool appears in discovery
- **WHEN** клиент выполняет handshake и `tools/list` сервера `calendar`
- **THEN** список содержит read-only инструмент `list_events` с полной JSON Schema

#### Scenario: Timed events overlap the requested interval
- **WHEN** CalDAV возвращает события, пересекающие валидный абсолютный интервал
- **THEN** инструмент возвращает нормализованные события в хронологическом порядке

#### Scenario: All-day event is returned
- **WHEN** CalDAV возвращает событие с датой без времени
- **THEN** результат сохраняет признак события на весь день и не придумывает локальное время начала

#### Scenario: Tomorrow is relative to a scheduled run
- **WHEN** вызов задаёт `target_day=tomorrow`, `relative_to` из `run.scheduled_at` и временную зону
- **THEN** сервер читает полный следующий локальный календарный день относительно фактического запуска

#### Scenario: Requested interval is invalid
- **WHEN** абсолютные границы некорректны либо относительный запрос не содержит необходимой опорной даты или зоны
- **THEN** инструмент отклоняет запрос до обращения к CalDAV

#### Scenario: Calendar contains no matching events
- **WHEN** CalDAV успешно отвечает без подходящих событий
- **THEN** инструмент возвращает успешный пустой список

## ADDED Requirements

### Requirement: Calendar server exposes only calendar capabilities
Сервер `calendar` SHALL объявлять `create_event` и `list_events` и MUST NOT объявлять scheduler, Telegram, GitHub, reporting, AI или workspace-инструменты. Оба инструмента SHALL использовать общий безопасный выбор целевого календаря и локальные CalDAV credentials.

#### Scenario: Calendar server is inspected
- **WHEN** клиент получает `tools/list` сервера `calendar`
- **THEN** обнаруживаются только календарные read/write операции с корректными аннотациями
