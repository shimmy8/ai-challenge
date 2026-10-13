## 1. Startup mode и конфигурация

- [x] 1.1 Добавить тесты разбора `review-pr` arguments, включая необязательный `--test-report`, и зарегистрировать новый неинтерактивный startup mode в `app.rs`; проверить обязательные repository/PR/output параметры и значение `not_run` без отчёта через `cargo test`.
- [x] 1.2 Реализовать `ReviewSettings` с приоритетом CLI → environment → defaults, безопасной валидацией provider/models/лимитов и redacted `Debug`; проверить тестами отсутствие secret в ошибках и отказ от чтения локальных config-файлов.
- [x] 1.3 Создать сфокусированную структуру `src/review/` и экспорт из `main.rs`, оставив entry point тонким; проверить сборку `cargo check`.

## 2. Получение pull request из GitHub

- [x] 2.1 Добавить в GitHub-клиент типы PR metadata и changed-file snapshot и paginated read-only запросы к pull request/files API; проверить mock-server тестами заголовки, pagination, base/head SHA и optional patch.
- [x] 2.2 Реализовать нормализацию repository-relative paths и статусов файлов; проверить тестами traversal, absolute paths, переименования, отсутствующий patch и некорректный JSON.
- [x] 2.3 Гарантировать, что acquisition не выполняет shell-команды и не загружает head-код для исполнения; проверить integration-тестом, что недоверенные title/body/path/patch остаются полями данных.

## 3. Review-профиль проектного корпуса

- [x] 3.1 Добавить тесты discovery-профиля для Markdown и Rust-кода, детерминированного порядка и неизменного Markdown-only поведения обычного `index`.
- [x] 3.2 Реализовать review discovery с canonical-root проверкой и denylist для `.git`, `target`, vendor/generated, env, credential, database и binary-файлов; проверить tempfile-тестами, включая symlink за пределы root.
- [x] 3.3 Добавить source-code документы и chunk metadata с достоверным structural context там, где он поддержан, и fixed fallback с line ranges; проверить стабильность chunk IDs и Unicode boundaries.
- [x] 3.4 Подключить environment-only embedding adapter и временный SQLite index к review flow; проверить fake-provider тестом batching, provenance, лимит 6000 символов и отсутствие записи credentials.

## 4. Отбор diff и пакетирование

- [x] 4.1 Реализовать классификацию binary/generated/lock/vendor и приоритет production source → configuration → tests → docs; проверить таблицей fixtures стабильный порядок и причины исключения.
- [x] 4.2 Реализовать defaults 50 файлов, 300000 общих символов и 40000 символов на файл с Unicode-safe hunk truncation; проверить граничные значения и coverage metadata.
- [x] 4.3 Реализовать batching до 60000 символов без разрыва hunk и детерминированный retrieval query из paths, hunk headers и identifiers; проверить, что каждый выбранный hunk попадает ровно в один пакет.

## 5. Test report и диагностический контекст

- [x] 5.1 Зафиксировать JSON-схему `test-report.json` со статусами `passed`, `failed`, `infrastructure_error` и строгими пределами полей; проверить parser-тестами валидные отчёты, неизвестный статус, oversized artifact и повреждённый JSON.
- [x] 5.2 Реализовать очистку ANSI/control characters, Unicode-safe ограничение 100000 символов и явный `truncated`; проверить fixtures с цветным выводом, Unicode, чрезмерным логом и prompt injection.
- [x] 5.3 Реализовать извлечение failed test names, panic locations, compiler error codes и repository-relative paths без исполнения содержимого; проверить Rust test-failure и compilation-failure fixtures.
- [x] 5.4 Дополнить retrieval query валидированными диагностическими идентификаторами только для `failed`; проверить, что `passed` не меняет обычный query, а `infrastructure_error` не выдаётся за дефект кода.

## 6. LLM-review и валидация

- [x] 6.1 Зафиксировать типизированную JSON-схему finding/result с test status и test-failure explanation и написать parser-тесты для всех severity/category/confidence, пустого результата и malformed provider responses.
- [x] 6.2 Реализовать batch prompt с явным разделением trusted instructions, PR data, test report и RAG contexts; проверить snapshot-тестом, что prompt-injection fixtures остаются внутри недоверенных секций и tool-calling не предлагается.
- [x] 6.3 Реализовать validation path и line range относительно snapshot/hunks, очистку недостоверной line-привязки и отклонение неизвестных файлов; проверить регрессионными тестами выдуманные пути и строки.
- [x] 6.4 Реализовать причинный контракт `confirmed`, `likely`, `unknown` с обязательными evidence IDs и отдельным `infrastructure_error`; проверить тестами прямую причину, неоднозначную диагностику, отсутствие данных и запрет выдуманного evidence.
- [x] 6.5 Реализовать агрегацию только по валидированным finding/evidence IDs, дедупликацию, повторную provenance-проверку, сортировку и default-лимит 30; проверить тестами дубли между пакетами и запрет новых неподтверждённых findings.
- [x] 6.6 Соединить batch retrieval, provider calls и aggregation с timeouts и безопасным error propagation; проверить fake chat/embedding providers для passed, failed, infrastructure_error, no-context, timeout и полностью невалидного ответа.

## 7. Выходные артефакты и CLI

- [x] 7.1 Реализовать JSON renderer и Markdown renderer с test status, coverage, base/head SHA, скрытым marker и обязательными review-разделами; проверить golden-тестами passed, failed с `Почему упали тесты`, infrastructure_error, пустое и частичное ревью.
- [x] 7.2 Добавить атомарную запись `--output` и `--json-output`, не затрагивающую предыдущие файлы при ошибке; проверить tempfile-тестами успешную замену и rollback при сбое.
- [x] 7.3 Собрать end-to-end `review-pr` flow на mock GitHub, test-report fixtures и fake providers и проверить, что он не создаёт сессии, профиль, задачу, MCP runtime или credential-файлы.

## 8. GitHub Actions pipeline

- [x] 8.1 Добавить `.github/workflows/ai-review.yml` на `pull_request_target` для `opened`, `reopened`, `synchronize` с same-repository guard и двумя jobs; проверить статическим regression-тестом, что `tests` checkout'ит точный head SHA с read-only permissions без secrets, а `review` checkout'ит точный base SHA и не исполняет PR-код.
- [x] 8.2 Реализовать test job с timeout, `cargo test`, сохранением exit code, always-очисткой/созданием/upload `test-report.json` и финальным восстановлением failed status; проверить workflow fixtures для passed, failed и infrastructure_error.
- [x] 8.3 Настроить review job с `needs: tests` и `if: always()`, скачать и проверить test-report artifact, передать `GITHUB_TOKEN`, `OPENAI_API_KEY`, model/limit variables только reviewer и пропускать fork без provider calls; проверить fixtures отсутствия secrets/write-token в test job и head checkout в review job.
- [x] 8.4 Добавить публикацию, которая создаёт или обновляет только bot-комментарий с `<!-- fox-llm-ai-review -->`, отражает test status и сохраняет красный test result; проверить mock GitHub API тестами create, update, duplicate avoidance, failed tests и review failure path.

## 9. Документация и итоговая проверка

- [x] 9.1 Обновить README: два изолированных job, repository secret/variables, test report, поддерживаемые события, defaults лимитов, fork policy, локальный запуск и предупреждение о `pull_request_target`; проверить, что документация не содержит реальных credentials.
- [x] 9.2 Добавить отчёт дня 32 в `reports/day32/` с архитектурой, примерами passed/failed review и командами проверки; проверить наличие только обезличенных fixtures и вывода.
- [x] 9.3 Выполнить `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings` и `cargo test`; исправить все ошибки до отметки задачи выполненной.
- [ ] 9.4 Выполнить два контролируемых same-repository PR smoke test: с прошедшими и упавшими тестами; подтвердить обязательный запуск review, красный test job при failed, причинный раздел, обновление одного sticky-комментария, partial coverage и отсутствие secrets в логах, затем записать результат в отчёт дня 32.
