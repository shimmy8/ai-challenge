## Purpose

Предоставляет воспроизводимый демонстрационный MCP-пайплайн, который получает публичные данные GitHub-репозитория, рассчитывает понятные метрики и сохраняет безопасный Markdown-отчёт.

## ADDED Requirements

### Requirement: Public repository metadata is available as structured data
Система SHALL предоставлять read-only MCP-инструмент, который принимает `owner/repository`, получает общедоступные метаданные и языки из GitHub REST API и возвращает нормализованный JSON с датами, звёздами, форками, подписчиками, открытыми issues, основной веткой и распределением языков. GitHub-токен MUST оставаться необязательным; если локальная переменная `GITHUB_TOKEN` задана, клиент SHALL использовать её только как чувствительный Bearer header для увеличения API rate limit. Система MUST NOT передавать токен модели, trace, логам или текстам ошибок и MUST NOT обращаться к private, traffic или security endpoints.

#### Scenario: Public repository is available
- **WHEN** инструмент получает корректное имя существующего публичного репозитория
- **THEN** он возвращает нормализованные метаданные и распределение языков как успешный структурированный результат

#### Scenario: Repository is missing or inaccessible
- **WHEN** GitHub возвращает not found, rate limit или сетевую ошибку
- **THEN** инструмент возвращает безопасную ошибку без предположения о значениях метрик

### Requirement: Public project activity is collected for a bounded period
Система SHALL предоставлять read-only MCP-инструмент активности, который принимает публичный репозиторий и период от 1 до 365 дней и возвращает доступные данные о коммитах, contributors, issues, pull requests, workflow runs и releases. Инструмент MUST учитывать пагинацию до окончания заданного периода в пределах безопасного верхнего лимита и MUST сообщать полноту отдельно для каждого источника. Инструмент MUST обрабатывать асинхронный `202 Accepted` статистических endpoint ограниченными повторами; для contributors SHALL использоваться bounded fallback по коммитам периода. Если отдельный источник остаётся недоступным или усечённым, инструмент MUST продолжить с явным warning, не подменяя отсутствующие данные нулём.

#### Scenario: Activity is collected
- **WHEN** GitHub возвращает данные за запрошенный период
- **THEN** инструмент возвращает структурированные счётчики и временные отметки, необходимые для расчёта метрик

#### Scenario: Requested period is outside bounds
- **WHEN** период меньше 1 или больше 365 дней
- **THEN** инструмент отклоняет аргументы до сетевого запроса

#### Scenario: Statistics are still being generated
- **WHEN** GitHub продолжает возвращать `202 Accepted` после ограниченных повторов
- **THEN** инструмент возвращает остальные доступные данные, помечает зависимые значения как unavailable и добавляет предупреждение о необходимости повторить запрос позже

### Requirement: Derived metrics are deterministic and transparent
Система SHALL предоставлять read-only MCP-инструмент, который принимает нормализованные metadata и activity, рассчитывает коммиты за период, число активных contributors, issue closure rate, PR merge rate, медианное время merge, CI success rate и release cadence и возвращает как значения, так и предупреждения о недоступных исходных данных. Инструмент MUST NOT подменять отсутствующие данные нулями.

#### Scenario: Complete inputs produce metrics
- **WHEN** metadata и activity содержат все необходимые значения
- **THEN** инструмент возвращает детерминированные метрики с единицами измерения и периодом расчёта

#### Scenario: Optional source is unavailable
- **WHEN** часть данных, например workflow runs, недоступна
- **THEN** соответствующая метрика имеет значение unavailable и сопровождается предупреждением, а остальные метрики рассчитываются

### Requirement: GitHub report is rendered from structured metrics
Система SHALL предоставлять read-only MCP-инструмент, который преобразует metadata, исходную activity и рассчитанные метрики в Markdown с разделами Overview, Development, Collaboration, Delivery, Data coverage и Warnings. Отчёт MUST показывать исходные счётчики рядом с производными значениями, различать полученные, рассчитанные, усечённые и недоступные данные и MUST NOT включать произвольные секреты или содержимое локальных файлов.

#### Scenario: Markdown report is produced
- **WHEN** инструмент получает корректные metadata и metrics
- **THEN** он возвращает непустой Markdown-отчёт с идентификатором репозитория, периодом и доступными метриками

### Requirement: Agent can compose the GitHub report pipeline
При запросе пользователя на GitHub-отчёт система SHALL позволять агенту сформировать план из доступных атомарных инструментов получения metadata, сбора activity, расчёта метрик, рендеринга и общего `save_report_to_file`. Результат каждого этапа SHALL передаваться следующему через ссылки пайплайна, а фактический порядок MCP-вызовов SHALL быть отражён в trace.

#### Scenario: End-to-end GitHub report succeeds
- **WHEN** агент сформировал валидный план для существующего публичного репозитория и пользователь подтвердил сохранение
- **THEN** система последовательно вызывает атомарные MCP-инструменты, создаёт Markdown-файл и возвращает успешный trace с итоговым путём
