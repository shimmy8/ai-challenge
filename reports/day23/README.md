# День 23. Реранкинг и фильтрация

## Цель

Эксперимент сравнивает два RAG-режима на тех же 10 контрольных вопросах, что и День 22:

- `baseline_rag`: embedding исходного вопроса и прямой top-K по cosine similarity;
- `enhanced_rag`: изолированный query rewrite, объединённый semantic score исходного и переписанного запросов, similarity threshold и детерминированный heuristic reranking.

Plain-режим без RAG не запускался повторно. Полный машинно-читаемый результат сохранён в [`comparison.json`](comparison.json).

## Настройки

| Параметр | Значение |
|---|---:|
| `candidate_k` до фильтрации | 20 |
| `final_k` после reranking | 5 |
| `min_similarity` | 0.40 |
| Бюджет контекста | 6000 Unicode-символов |
| Semantic weight | 0.70 |
| Content lexical coverage weight | 0.20 |
| `title`/`section` coverage weight | 0.10 |

Query rewrite выполнялся текущей chat-моделью с temperature 0, без истории, памяти, профиля, пользовательских mode-инструкций и MCP tools. Retrieval всегда учитывал также исходный вопрос, поэтому rewrite не заменял пользовательский запрос.

## Воспроизведение

```bash
cargo run -- rag-eval \
  --questions reports/day22/control-questions.json \
  --output reports/day23/comparison.json \
  --candidate-k 20 \
  --final-k 5 \
  --min-similarity 0.40
```

Запуск требует существующего structural-индекса, локальной embedding-конфигурации и настроенного chat provider. Итоговый JSON записывается атомарно и с приватными правами доступа.

## Агрегированный результат

| Метрика | `baseline_rag` | `enhanced_rag` | Изменение |
|---|---:|---:|---:|
| Mean fact coverage | 77.5% | 77.5% | 0.0 п.п. |
| Mean source recall | 90.0% | 90.0% | 0.0 п.п. |
| Input tokens | 8,796 | 9,314 | +518 (+5.9%) |
| Output tokens | 1,185 | 1,466 | +281 (+23.7%) |
| Всего tokens | 9,981 | 10,780 | +799 (+8.0%) |
| Суммарная длительность | 45,449 ms | 58,079 ms | +12,630 ms (+27.8%) |
| Generation requests | 10 | 10 | 0 |
| `no_relevant_context` | 0 | 0 | 0 |

По выбранным автоматическим метрикам enhanced pipeline не улучшил и не ухудшил итоговое качество. Дополнительные rewrite-вызовы увеличили расход токенов и длительность.

## Результаты по вопросам

`Facts` и `sources` ниже — доли от 0 до 100%. `Before → after` показывает число enhanced-кандидатов до и после similarity threshold; в generation передавалось не более пяти результатов.

| ID | Baseline facts | Enhanced facts | Baseline sources | Enhanced sources | Before → after | Наблюдение |
|---|---:|---:|---:|---:|---:|---|
| `chunking-defaults` | 0.0% | 0.0% | 0.0% | 0.0% | 20 → 2 | Фильтр удалил 18 слабых кандидатов, но нужных default-значений в оставшемся контексте не было. |
| `day21-strategy-comparison` | 33.3% | 33.3% | 100.0% | 100.0% | 20 → 9 | Нужный отчёт поднялся на первое место, но в top-5 также попал нерелевантный global-invariants источник. |
| `memory-layers` | 100.0% | 100.0% | 100.0% | 100.0% | 20 → 6 | Enhanced top-5 целиком состоит из `agent-memory`; baseline включал один `task-lifecycle` чанк. |
| `task-phases` | 100.0% | 100.0% | 100.0% | 100.0% | 20 → 20 | Reranking сохранил качество и сделал top-5 однороднее по `task-lifecycle`. |
| `profile-session-boundary` | 100.0% | 100.0% | 100.0% | 100.0% | 20 → 20 | Метрики и смысл ответа не изменились. |
| `calendar-missing-time` | 100.0% | 100.0% | 100.0% | 100.0% | 20 → 18 | Enhanced исключил нерелевантный `task-lifecycle` чанк из финального top-5. |
| `mcp-call-limits` | 66.7% | 66.7% | 100.0% | 100.0% | 20 → 20 | Enhanced поднял точный scenario про tool-call limit, но один scheduled-pipeline источник остался нерелевантным. |
| `scheduled-digest-architecture` | 100.0% | 100.0% | 100.0% | 100.0% | 20 → 20 | В enhanced top-5 появился релевантный отчёт Дня 18; итоговая оценка не изменилась. |
| `telegram-unknown-delivery` | 75.0% | 75.0% | 100.0% | 100.0% | 20 → 20 | Порядок и набор основных источников практически не изменились. |
| `github-report-sections` | 100.0% | 100.0% | 100.0% | 100.0% | 20 → 20 | Оба режима нашли правильную спецификацию и полный список разделов. |

В среднем threshold оставил 15.5 из 20 кандидатов, то есть удалил 22.5% candidate pool. Для шести вопросов порог не удалил ни одного кандидата; его влияние оказалось сосредоточено на первых, наиболее проблемных запросах.

## Ручной разбор

### Улучшения retrieval

- Query rewrite и lexical/metadata score сделали top-5 тематически однороднее для `memory-layers`, `task-phases` и `calendar-missing-time`.
- Для `day21-strategy-comparison` нужный источник переместился с четвёртой позиции baseline на первую позицию enhanced.
- Для `mcp-call-limits` enhanced добавил в финальный контекст точный scenario достижения лимита tool calls.
- Threshold эффективно сократил явно слабый хвост для `chunking-defaults`: с 20 кандидатов до 2 без передачи нерелевантных фрагментов модели.

### Регрессии и ограничения

- Улучшения порядка источников не дали прироста fact coverage или source recall на этом контрольном наборе.
- В `day21-strategy-comparison` heuristic reranker поднял нерелевантный чанк `global-invariants`; в `mcp-call-limits` сохранился нерелевантный scheduled-pipeline источник. Простое token overlap не гарантирует предметную релевантность.
- `chunking-defaults` остался неотвеченным в обоих режимах. Фильтрация корректно уменьшила шум, но не может восстановить факт, отсутствующий в найденных чанках.
- Порог 0.40 почти не влиял на шесть вопросов с высокой общей similarity. Его нельзя считать универсальным: значение зависит от embedding model и корпуса.
- Один прогон сравнивает baseline с полным enhanced pipeline, поэтому отдельно приписать эффект query rewrite, threshold или reranking нельзя. Для причинного анализа нужны дополнительные ablation-режимы.

## Как интерпретировать качество

Fact coverage проверяет наличие заранее заданных строковых фрагментов после Unicode-aware нормализации. Source recall проверяет присутствие ожидаемых путей среди фактически переданных чанков. Это прозрачные воспроизводимые эвристики, а не полноценная семантическая оценка ответа. Поэтому ручной разбор порядка и тематической чистоты источников обязателен даже при одинаковых агрегатах.

## Проверка реализации

- `cargo fmt --all -- --check` — успешно.
- `cargo clippy --all-targets --all-features -- -D warnings` — успешно.
- `cargo test` — успешно, 188 тестов пройдено.
- `openspec validate add-rag-rewrite-reranking --strict` — успешно.
