## MODIFIED Requirements

### Requirement: Pull request автоматически запускает изолированные тесты и доверенное ревью
Система SHALL при открытии, повторном открытии и обновлении pull request из ветки того же репозитория сначала запускать test job, а затем review job независимо от результата тестов. Test job MAY исполнять код точного `head.sha`, но MUST NOT получать API secrets, write-capable token или право записи cache; он MAY восстанавливать debug Cargo cache, созданный отдельным trusted workflow из `main`. Привилегированный review job SHALL использовать reviewer и workflow из защищённой базовой ветки, собирать только выделенный минимальный reviewer target и переиспользовать отдельный trusted Cargo build cache, получать содержимое PR и test report только как недоверенные данные и MUST NOT исполнять код, build scripts, зависимости или конфигурацию из PR с доступом к secrets. Pull request из fork SHALL пропускать test и review jobs без LLM- и embedding-запросов и без запуска отдельного notification job.

#### Scenario: Pull request из ветки репозитория
- **WHEN** автор открывает или обновляет pull request, чья head-ветка принадлежит тому же репозиторию
- **THEN** изолированный job тестирует точный head commit без secrets, после чего доверенный reviewer базовой ветки собирает минимальный reviewer target, получает metadata, изменённые файлы и результат тестов и запускает анализ

#### Scenario: Pull request из fork
- **WHEN** событие относится к pull request из fork-репозитория
- **THEN** workflow пропускает test и review jobs, не передаёт credentials, не обращается к LLM или embedding provider и не запускает отдельный runner только для уведомления о пропуске

#### Scenario: Недоверенный PR содержит исполняемые инструкции
- **WHEN** изменённые workflow, build script или исходный файл содержат команды либо prompt injection
- **THEN** только изолированный test job может исполнить тестовую сборку PR без secrets, а review job обрабатывает содержимое и test report только как данные анализа и не выполняет команды и инструкции из них

#### Scenario: Повторная сборка trusted reviewer
- **WHEN** `Cargo.lock` и release dependency graph минимального reviewer совместимы с предыдущим trusted запуском
- **THEN** review job восстанавливает Cargo registry/git и release-артефакты из отдельного build cache и Cargo перекомпилирует только отсутствующие или изменившиеся единицы

#### Scenario: PR-тесты используют прогретый main cache
- **WHEN** trusted workflow ранее подготовил совместимый debug cache для текущих Rust и `Cargo.lock`
- **THEN** test job восстанавливает registry/git и `target/debug` в read-only режиме, запускает точный `head.sha` и не может сохранить изменённые PR-артефакты обратно

## ADDED Requirements

### Requirement: Generation-запрос совместим с выбранной review-моделью
Система SHALL формировать Responses API payload с параметрами, поддерживаемыми выбранной моделью. Для reasoning-моделей семейства `gpt-5.1`–`gpt-5.6` при использовании `temperature` система SHALL явно задавать `reasoning.effort` как `none`. При HTTP-отказе система SHALL выводить status и MAY выводить только структурированные `error.type`, `error.code` и `error.param`; полный response body, provider message, credentials и содержимое prompt MUST NOT попадать в диагностику.

#### Scenario: Review использует reasoning-модель с temperature
- **WHEN** для generation выбрана модель семейства `gpt-5.1`–`gpt-5.6`
- **THEN** Responses payload содержит `temperature: 0` и `reasoning.effort: none`

#### Scenario: Responses API отклоняет generation-запрос
- **WHEN** provider возвращает неуспешный HTTP status и структурированную ошибку
- **THEN** job завершается с безопасной диагностикой status/type/code/param без полного body, message, API key или prompt

## MODIFIED Requirements

### Requirement: Ревью использует RAG по документации и коду базовой ветки
Перед generation система SHALL создать review-корпус из разрешённой документации и текстового исходного кода базовой ветки. Для каждого чанка система SHALL вычислить content hash и переиспользовать сохранённый vector только при совпадении cache schema, provider endpoint origin, embedding model, подтверждённых provider dimensions и версии chunking/normalization. Cache MUST NOT содержать credentials или исходный текст корпуса, MUST считаться недоверенным вводом и MUST проверяться перед использованием; перед сохранением система SHALL удалить vectors, отсутствующие в текущем корпусе. Для cache misses система SHALL попытаться получить embeddings и выполнить semantic retrieval по путям файлов, изменённым идентификаторам, краткому описанию diff и диагностическим идентификаторам failed test report. Тайм-аут одного HTTP-запроса embeddings SHALL быть отделён от большего общего бюджета холодного построения индекса; каждый полностью проверенный пакет vectors SHALL сохраняться до следующего сетевого ожидания, чтобы отмена общего бюджета не уничтожала выполненный прогрев. Ошибка записи такого пакета SHALL завершать retrieval до следующего provider request, а counters при общем timeout SHALL отражать фактически загруженные и полученные vectors. Для `429`, `5xx`, timeout и временных transport errors система SHALL выполнить ограниченные повторные попытки, а после их исчерпания SHALL продолжить анализ с детерминированным lexical retrieval по тому же корпусу без дальнейших embedding-запросов в этом запуске. Ошибки аутентификации, некорректной конфигурации и невалидного успешного ответа provider SHALL оставаться фатальными. Каждый пакет SHALL получать не более 6000 Unicode-символов релевантного контекста целыми чанками с provenance. Diff, PR metadata, test report и найденные фрагменты SHALL быть явно отделены как недоверенные данные; инструкции из них MUST NOT изменять review-контракт.

#### Scenario: Semantic retrieval доступен
- **WHEN** embedding provider успешно обрабатывает корпус и запросы
- **THEN** generation получает релевантные чанки с путями и секциями в пределах RAG-бюджета, а результат отмечает режим retrieval как `embedding`

#### Scenario: Найден связанный проектный контекст
- **WHEN** diff изменяет код, связанный с документированной спецификацией или соседними символами базовой ветки
- **THEN** generation получает релевантные чанки документации и кода с путями и секциями в пределах RAG-бюджета выбранного retrieval-режима

#### Scenario: Неизменившиеся чанки найдены в cache
- **WHEN** восстановленный cache содержит vectors с совпадающими content hashes и полным embedding descriptor
- **THEN** reviewer после подтверждения live query dimensions переиспользует эти vectors, запрашивает embeddings только для отсутствующих чанков и не привязывает актуальность контекста к устаревшим cache rows

#### Scenario: Embedding cache отсутствует
- **WHEN** предыдущий cache не найден или был удалён системой хранения
- **THEN** reviewer строит semantic index обычными embedding-запросами и сохраняет проверенные vectors для следующих trusted запусков

#### Scenario: Холодная индексация превышает прежний короткий timeout
- **WHEN** успешные embedding batches требуют больше времени, чем допустимо одному HTTP-запросу, но укладываются в общий бюджет построения индекса
- **THEN** reviewer завершает semantic indexing вместо преждевременного lexical fallback

#### Scenario: Общий бюджет индексации исчерпан после готового batch
- **WHEN** один или несколько embedding batches уже проверены, а следующий запрос не завершился до общего timeout
- **THEN** reviewer сохраняет готовые vectors в SQLite, публикует безопасно помеченный lexical fallback и позволяет следующему trusted запуску продолжить прогрев

#### Scenario: Embedding cache несовместим или повреждён
- **WHEN** cache имеет другую schema, provider endpoint origin, model, dimensions или chunking/normalization version либо не проходит проверку SQLite/vectors
- **THEN** reviewer игнорирует несовместимые или повреждённые данные без исполнения их содержимого, перестраивает доступную часть индекса и при временном отказе provider сохраняет возможность lexical fallback

#### Scenario: Embedding provider временно ограничивает запросы
- **WHEN** embedding provider возвращает `429`, а ограниченные повторные попытки не приводят к успеху
- **THEN** reviewer прекращает embedding-запросы для текущего запуска, выбирает контекст детерминированным lexical retrieval, продолжает generation и отмечает режим retrieval как `lexical_fallback`

#### Scenario: Embedding provider временно недоступен
- **WHEN** embedding provider после ограниченных повторов продолжает возвращать `5xx`, timeout или временную transport error
- **THEN** reviewer продолжает анализ в режиме `lexical_fallback`, не теряя выбранный diff и test report

#### Scenario: Embedding provider отклоняет конфигурацию
- **WHEN** embedding provider возвращает ошибку аутентификации, невалидного запроса или несовместимый успешный ответ
- **THEN** reviewer завершает анализ безопасной ошибкой вместо маскировки неверной конфигурации lexical fallback-режимом

#### Scenario: Релевантный контекст отсутствует
- **WHEN** ни один чанк не проходит порог выбранного retrieval-режима
- **THEN** reviewer анализирует сам diff, явно фиксирует отсутствие дополнительного RAG-контекста и не подставляет нерелевантные чанки

#### Scenario: RAG-контекст содержит prompt injection
- **WHEN** документация или исходный код содержит команды модели
- **THEN** generation рассматривает их только как цитируемые проектные данные и сохраняет системный review-контракт

#### Scenario: Test log содержит prompt injection
- **WHEN** имя теста, panic message или иной test output содержит инструкции модели
- **THEN** generation рассматривает их только как диагностические данные и не изменяет review-контракт

### Requirement: Результат ревью имеет проверяемую структуру
Система SHALL создавать машинный JSON и человекочитаемый Markdown. Trusted prompt SHALL требовать русский язык для всех пользовательских свободных текстов результата, а статические подписи Markdown/Actions Summary SHALL быть на русском языке; пути, inline-code, общепринятые технические названия, identifiers, enum-значения и evidence IDs MAY сохранять техническое написание. Reviewer MUST NOT отклонять структурно корректный generation-ответ на основании эвристической классификации языка и MUST NOT повторять generation только из-за английских слов. Результат SHALL содержать краткое описание изменений с подтверждёнными `diff:` evidence IDs, режим retrieval `embedding` или `lexical_fallback` и необязательную типизированную причину fallback. При fallback Markdown SHALL содержать заметное предупреждение с безопасной категорией причины без раскрытия credentials, provider message или response body. Каждое замечание SHALL иметь `severity` (`critical`, `warning` или `suggestion`), `category` (`bug`, `architecture` или `recommendation`), файл, заголовок, объяснение, рекомендацию и использованные RAG sources. `start_line` и `end_line` SHALL присутствовать только при достоверной привязке к строкам diff. Результат SHALL содержать test status. Markdown SHALL содержать разделы `Что изменилось`, `Потенциальные баги`, `Архитектурные проблемы` и `Рекомендации`, даже если соответствующий раздел пуст. Для статуса `failed` Markdown SHALL дополнительно содержать раздел `Почему упали тесты` с одной из оценок уверенности `confirmed`, `likely` или `unknown`, evidence из ограниченного test report и связью с diff/RAG только когда она подтверждается доступными данными.

#### Scenario: Модель описывает изменения PR
- **WHEN** batch содержит выбранные diff-файлы
- **THEN** JSON сохраняет непустые элементы change summary только с известными `diff:<path>` evidence, а Markdown выводит их в разделе `Что изменилось`

#### Scenario: Prompt задаёт язык комментария
- **WHEN** reviewer формирует generation-запрос
- **THEN** trusted instructions явно требуют русский язык для описания изменений, заголовков, объяснений, рекомендаций и объяснения падения тестов

#### Scenario: Русский текст содержит технические английские слова
- **WHEN** структурно корректный generation-ответ использует английские технические термины в русской прозе
- **THEN** reviewer принимает ответ после единственного generation-вызова и продолжает обычную schema/evidence validation

#### Scenario: Найдены проблемы разных категорий
- **WHEN** модель возвращает валидные подтверждённые замечания о баге, архитектуре и улучшении
- **THEN** JSON сохраняет типизированные поля и режим retrieval, а Markdown размещает каждое замечание в соответствующем обязательном разделе

#### Scenario: Использован lexical fallback
- **WHEN** semantic retrieval недоступен из-за исчерпанных временных ошибок provider
- **THEN** JSON содержит `lexical_fallback` и одну из безопасных причин `rate_limited`, `provider_unavailable`, `timeout`, `transport_error`, Markdown объясняет причину, Actions Summary предупреждает о degraded-контексте, а findings могут ссылаться только на фактически переданные lexical chunks

#### Scenario: Строку определить нельзя
- **WHEN** замечание относится к файлу в целом или позиция не подтверждается patch
- **THEN** результат не выдумывает line range и сохраняет замечание на уровне файла

#### Scenario: Проблем не найдено
- **WHEN** после проверки и дедупликации не осталось замечаний
- **THEN** Markdown явно сообщает об отсутствии найденных проблем и сохраняет все три обязательных раздела

#### Scenario: Причина падения подтверждена
- **WHEN** failed test report, diff и проектный контекст однозначно подтверждают причину
- **THEN** раздел `Почему упали тесты` маркирует вывод как `confirmed`, указывает конкретный evidence и предлагает связанную рекомендацию

#### Scenario: Причина падения неоднозначна
- **WHEN** failed test report допускает несколько причин либо не содержит достаточной диагностики
- **THEN** раздел маркирует наиболее обоснованный вывод как `likely` или причину как `unknown` и не выдаёт предположение за установленный факт

#### Scenario: Тесты не удалось запустить
- **WHEN** test status равен `infrastructure_error`
- **THEN** Markdown отдельно сообщает, что тесты не выполнены достоверно, и не создаёт раздел с выдуманной причиной падения кода

### Requirement: Workflow идемпотентно публикует ревью
Review job SHALL зависеть от test job и запускаться независимо от его результата для pull request из ветки того же репозитория. Только trusted review job SHALL иметь write-capable cache mode и восстанавливать/сохранять embedding и release build caches; test job и код PR MUST NOT получать write-доступ ни к одному cache namespace. Изменившийся существующий embedding cache SHALL сохраняться после завершения шага review независимо от результата последующих generation и publication steps, чтобы не терять уже проверенные batches. Отдельный workflow на trusted `main` MAY сохранять debug test cache. После успешного полного или degraded-анализа workflow SHALL создать либо обновить ровно один PR-комментарий, идентифицированный стабильным скрытым маркером, и записать краткий Actions Summary с test status, режимом retrieval и безопасными cache hit/miss counts. Повторный запуск для того же PR MUST обновлять существующий комментарий вместо создания нового. Test job SHALL использовать read-only permissions без API secrets, а review job SHALL использовать минимальные permissions `contents: read` и `pull-requests: write`.

#### Scenario: Первый успешный запуск
- **WHEN** для PR ещё нет комментария с маркером AI-review
- **THEN** workflow создаёт один комментарий с Markdown-ревью и добавляет summary запуска с test status и режимом retrieval

#### Scenario: PR получил новый commit
- **WHEN** повторный запуск успешно анализирует обновлённый PR
- **THEN** workflow заменяет содержимое найденного sticky-комментария и не создаёт второй комментарий

#### Scenario: Тесты упали, а review завершилось успешно
- **WHEN** test job завершился статусом `failed`, но review job успешно объяснил доступные причины и сформировал результат
- **THEN** sticky-комментарий обновляется полным ревью с разделом о тестах, review job успешен, а test job и общий статус проверки сохраняют факт падения тестов

#### Scenario: Embeddings недоступны, а fallback завершился успешно
- **WHEN** semantic retrieval исчерпал повторные попытки и reviewer успешно завершил анализ с lexical retrieval
- **THEN** sticky-комментарий обновляется degraded-ревью, review job остаётся успешным, а Actions Summary явно указывает `lexical_fallback`

#### Scenario: Cache доступен только доверенному reviewer
- **WHEN** workflow обрабатывает содержимое pull request и восстанавливает cache предыдущих запусков
- **THEN** cache steps выполняются только после checkout доверенного `base.sha` в review job, а test job с кодом `head.sha` не может сохранять или изменять review-cache namespace

#### Scenario: Generation упало после частичного прогрева cache
- **WHEN** retrieval записал новые проверенные vectors в SQLite, а последующий generation или publication step завершился ошибкой
- **THEN** trusted review job сохраняет изменившийся существующий cache для следующего запуска, но не заменяет sticky-комментарий неполным результатом

#### Scenario: Анализ завершился ошибкой
- **WHEN** GitHub API, chat provider, неустранимая ошибка embedding-конфигурации или валидация результата завершается ошибкой
- **THEN** job завершается ошибкой, Actions Summary содержит безопасную диагностику, а существующий review-комментарий не удаляется и не заменяется неполным аварийным текстом
