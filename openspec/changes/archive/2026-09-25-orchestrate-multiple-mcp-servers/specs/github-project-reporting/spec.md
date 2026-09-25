## MODIFIED Requirements

### Requirement: Agent can compose the GitHub report pipeline
При запросе GitHub-отчёта система SHALL позволять агенту сформировать межсерверный план: получить metadata и activity через сервер `github`, рассчитать метрики и отрендерить Markdown через `reporting`, затем при необходимости сохранить файл через `workspace`. Результат каждого этапа SHALL передаваться следующему через ссылки pipeline, а trace MUST отражать квалифицированные имена, серверы и фактический порядок вызовов.

#### Scenario: End-to-end GitHub report succeeds
- **WHEN** агент сформировал валидный план для публичного репозитория и пользователь подтвердил сохранение
- **THEN** система последовательно вызывает инструменты трёх серверов, создаёт Markdown-файл и возвращает успешный trace с итоговым путём

#### Scenario: Reporting step fails before file output
- **WHEN** получение GitHub-данных успешно, но расчёт или рендеринг завершается ошибкой
- **THEN** `workspace__save_report` не вызывается, а trace сохраняет успешные исходные и неуспешный преобразующий шаги в правильном порядке

## ADDED Requirements

### Requirement: GitHub and reporting responsibilities are isolated
Сервер `github` SHALL объявлять только инструменты получения публичных repository metadata и activity, а сервер `reporting` SHALL объявлять только детерминированные инструменты расчёта GitHub-метрик и рендеринга Markdown. Reporting MUST NOT выполнять сетевые GitHub-запросы или запись файлов.

#### Scenario: Server catalogs are inspected
- **WHEN** клиент получает `tools/list` обоих серверов
- **THEN** исходные, вычислительные и файловые инструменты находятся только на назначенных серверах
