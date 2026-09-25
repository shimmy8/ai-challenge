## REMOVED Requirements

### Requirement: Scheduler tools expose a bounded job contract
**Reason**: Календарно-специфичные tools и поля задания заменяются универсальным scheduled MCP-pipeline.
**Migration**: Календарная сводка создаётся через общий `orchestrator__create_job` с шагами Calendar, AI и Telegram.

### Requirement: Jobs and runs survive process restarts
**Reason**: Гарантия переносится в общую capability `scheduled-pipeline-jobs` и применяется ко всем pipeline.
**Migration**: Новые задания хранят общую pipeline-модель и историю запусков в SQLite orchestrator.

### Requirement: Each due occurrence is claimed at most once locally
**Reason**: Гарантия переносится без календарной специализации в общий orchestrator.
**Migration**: Атомарный claim и восстановление interrupted runs реализуются для generic job/run.

### Requirement: Job lifecycle is controllable and observable
**Reason**: Календарные lifecycle-инструменты заменяются общими инструментами orchestrator.
**Migration**: Используются `create_job`, `list_jobs`, `pause_job`, `resume_job`, `delete_job`, `run_job_now` и `get_job_history`.

### Requirement: Background execution has an explicit availability boundary
**Reason**: Граница доступности становится общей гарантией scheduled pipeline.
**Migration**: Orchestrator выполняет задания только пока его отдельный процесс запущен и отражает восстановление в общей истории.

## ADDED Requirements

### Requirement: Calendar digest is a generic scheduled pipeline
Периодическая календарная сводка SHALL представляться обычным заданием orchestrator со стадиями чтения событий, изолированной AI-обработки и Telegram-доставки. Задание MUST NOT использовать специальный calendar runner или calendar-specific поля хранения.

#### Scenario: Daily calendar digest is scheduled
- **WHEN** пользователь подтверждает ежедневный pipeline `calendar__list_events` → `ai__generate_text` → `telegram__send_message`
- **THEN** orchestrator сохраняет и выполняет его по общим правилам scheduled pipeline

#### Scenario: Calendar source fails
- **WHEN** чтение CalDAV завершается ошибкой
- **THEN** AI и Telegram шаги не выполняются, а история содержит failed для источника и skipped для последующих шагов
