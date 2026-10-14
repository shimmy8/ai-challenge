# День 32. Автоматизация ревью кода

## Результат

Добавлен автоматический pipeline первичного AI-ревью pull request. Он получает
metadata и paginated patches из GitHub API, ограничивает diff, извлекает
релевантный контекст из документации и Rust-кода базовой ветки и публикует
структурированный Markdown-комментарий.

## Архитектура и границы доверия

```text
pull_request_target
├── tests: head.sha, cargo test, contents:read, без secrets
│   └── очищенный ограниченный test-report.json
└── review: base.sha, OPENAI_API_KEY, pull-requests:write
    ├── GitHub API → metadata + changed files + patches
    ├── selection/batching → прозрачный partial coverage
    ├── content-addressed SQLite cache → только hashes + vectors
    ├── semantic RAG или детерминированный lexical fallback
    ├── LLM JSON → path/line/evidence validation
    └── один sticky-комментарий
```

PR title/body, diff, test log и RAG chunks считаются недоверенными данными.
Review job не checkout'ит head SHA и не выполняет код, build scripts или
конфигурацию PR. Fork PR пропускаются до provider calls.

Reviewer выделен в workspace crate `crates/fox-review`; privileged job собирает
только `cargo build --release --package fox-review --bin fox-review`. Корневой
`fox-llm review-pr` сохранён как совместимый адаптер.

Cache `.ai-review-cache/review-rag.db` восстанавливается и сохраняется только в
trusted review job. Namespace проверяет schema, OpenAI origin, embedding model,
runtime dimensions и chunking version. Повреждение даёт cache miss, удалённые
chunks pruning'уются, исходный текст и credentials в SQLite не записываются.
Изменившийся cache сохраняется даже при последующей ошибке generation или
публикации комментария, чтобы следующий trusted запуск не повторял уже
выполненный прогрев.

Для временных embedding-ошибок выполняются максимум три попытки. После
исчерпания `429`, `5xx`, timeout или transport error circuit breaker запрещает
следующие embedding calls и включает `lexical_fallback` для всех batches.
Permanent provider errors остаются фатальными. `review.json`, Markdown и
Actions Summary явно показывают retrieval mode и cached/new counters.
Один HTTP-запрос эмбеддингов ограничен 30 секундами, а холодное построение
индекса — пятью минутами. Каждый готовый batch сразу записывается в SQLite,
поэтому при исчерпании общего бюджета следующий запуск продолжает прогрев.

Пользовательский текст комментария всегда русский: prompt требует русский для
описания изменений, заголовков, объяснений и рекомендаций, validator отклоняет
английскую прозу даже с единичной кириллической буквой и один раз повторяет
нарушивший контракт ответ. Второй нерусский ответ не публикуется; статические
подписи комментария и Actions Summary также русифицированы.

## Пример при успешных тестах

```markdown
<!-- fox-llm-ai-review -->
# AI-ревью PR #42

**Тесты:** `пройдены`

## Потенциальные баги
Не обнаружено.

## Архитектурные проблемы
Не обнаружено.

## Рекомендации
- **предложение: Упростить обработку ошибки** (`src/example.rs:18`) — ...
```

## Пример при упавших тестах

```markdown
**Тесты:** `упали`

## Почему упали тесты

**Уверенность:** `вероятно` — тест `parser::rejects_empty` падает рядом с
изменённой проверкой входных данных; прямого подтверждения причины недостаточно.
```

`infrastructure_error` выводится отдельно и не представляется как дефект кода.
При fallback Markdown дополнительно содержит предупреждение, а sticky-комментарий
всё равно создаётся или обновляется. Для fork обе содержательные jobs имеют
статус skipped; отдельная `fork-notice` job удалена.

## Ограничения

- 50 файлов;
- 300000 Unicode-символов diff суммарно;
- 40000 символов на файл;
- 60000 символов на batch;
- 6000 символов RAG-контекста;
- 30 итоговых замечаний;
- 100000 символов очищенного test log.

## Проверка

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
node --test .github/scripts/publish-review.test.js
openspec validate harden-ai-review-workflow --strict
```

Контролируемые smoke tests в GitHub должны отдельно проверить PR с успешными и
упавшими тестами: review запускается в обоих случаях, failed test job остаётся
красным, обновляется один sticky-комментарий, partial coverage видим, а secrets
не появляются в логах. До фактического запуска этих PR проверка не считается
выполненной.
