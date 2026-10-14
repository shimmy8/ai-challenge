## Why

Автоматическое ревью дня 32 сейчас полностью срывается при временном ограничении embedding API: ответ `429 Too Many Requests` завершает `review` до обращения к chat-модели. Одновременно привилегированный job тратит время на сборку всего интерактивного `fox-llm`, а отдельный `fork-notice` запускает runner только ради сообщения о пропуске fork PR.

## What Changes

- Сделать embedding-запросы устойчивыми к временным отказам: ограниченно повторять `429` и `5xx` с учётом `Retry-After`, а после исчерпания попыток продолжать review с детерминированным lexical retrieval по тому же доверенному корпусу.
- Переиспользовать проверенные embeddings неизменившихся чанков между CI-запусками через content-addressed SQLite cache, который восстанавливается и сохраняется только trusted review job посредством GitHub Actions cache.
- Сохранять изменившийся trusted RAG cache даже при последующей ошибке generation или публикации, чтобы не терять уже проверенные embedding batches.
- Явно отражать в JSON, Markdown и Actions Summary, использован ли embedding или degraded lexical режим, а также число переиспользованных и новых vectors, не выдавая degraded review за полный embedding-backed результат.
- Добавить в JSON и Markdown краткое проверяемое описание изменений PR и безопасную типизированную причину перехода на lexical fallback.
- Разделить короткий timeout одного embedding HTTP-запроса и достаточный бюджет холодного построения semantic index, сохраняя каждый готовый пакет vectors до возможной отмены запуска.
- Запрашивать русский пользовательский текст AI-комментария через trusted prompt без эвристического language-validator и повторного generation-запроса, чтобы технические английские слова не срывали готовое review.
- Выделить минимальный CI-binary `fox-review`, который содержит только acquisition, test-report, retrieval, LLM analysis и rendering, и собирать в review job только этот target с минимальным feature/dependency graph.
- Переиспользовать Cargo registry/git и release-артефакты минимального reviewer между trusted CI-запусками, чтобы повторный review не перекомпилировал неизменившийся dependency graph.
- Прогревать debug Cargo cache доверенным workflow на `main`, а PR test job давать только read-only restore этого cache, не позволяя недоверенному head-коду публиковать артефакты.
- Синхронизировать Responses payload review-adapter с поддерживаемыми параметрами reasoning-моделей и выводить при отказе API только безопасные структурированные поля ошибки.
- Сохранить существующий интерактивный `fox-llm` и его команду `review-pr` как совместимый entry point, делегирующий общей review-библиотеке.
- **BREAKING**: удалить отдельный job `fork-notice` и больше не создавать Actions Summary для fork PR; guards на `tests` и `review` по-прежнему должны исключать выполнение кода и provider calls для fork.
- Обновить регрессионные тесты workflow и review flow для cache reuse/invalidation, retry, fallback, минимальной сборки и отсутствия лишнего fork job.

## Capabilities

### New Capabilities

Нет.

### Modified Capabilities

- `automated-pr-code-review`: embeddings неизменившихся чанков переиспользуются между trusted CI-запусками, embedding API становится best-effort источником недостающего семантического контекста с прозрачным lexical fallback; CI собирает отдельный минимальный reviewer target; fork PR безопасно пропускается без отдельного notification job.

## Impact

- Затрагиваются текущий `src/review/`, новый `crates/fox-review`, границы общих provider/model типов, Cargo workspace/targets, `src/main.rs`, `.github/workflows/ai-review.yml`, review-cache SQLite schema, отдельный Cargo build cache, workflow regression tests и документация дня 32.
- Формат review JSON и Markdown расширяется описанием изменений и типизированной причиной fallback; существующие finding-поля и sticky marker сохраняются.
- Внешние chat-запросы остаются обязательными для AI-review; fallback устраняет только обязательность успешного embedding-запроса.
- Схемы пользовательских данных, пользовательские конфигурации, MCP и интерактивный сценарий не меняются; новый cache содержит только hashes, descriptor metadata и vectors, но не credentials или исходный текст корпуса.
