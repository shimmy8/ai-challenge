## ADDED Requirements

### Requirement: Remote Ollama is a separate provider
Система SHALL предлагать `Ollama Remote` как отдельного провайдера наряду с OpenAI, Claude и локальным `Ollama`. Выбор `Ollama Remote` MUST NOT изменять endpoint, модель или сохранённые настройки локального `Ollama` и облачных провайдеров. `Ollama Remote` MUST NOT запрашивать, сохранять или отправлять API-ключ.

#### Scenario: User selects Remote Ollama
- **WHEN** пользователь выбирает `Ollama Remote` в списке провайдеров
- **THEN** приложение активирует отдельную конфигурацию удалённого Ollama без изменения конфигурации локального `Ollama`

#### Scenario: User switches between local and remote Ollama
- **WHEN** пользователь переключается с `Ollama Remote` на локальный `Ollama` или обратно
- **THEN** каждый провайдер восстанавливает собственные endpoint и модель без запроса API-ключа

### Requirement: Remote Ollama endpoint is explicit and persistent
Система SHALL требовать для `Ollama Remote` явно настроенный HTTP(S) base endpoint, SHALL сохранять его между запусками и MUST отклонять некорректный URL до выполнения сетевого запроса. Допустимый endpoint MUST содержать схему HTTP(S) и host и MUST NOT содержать credentials, query или fragment.

#### Scenario: Remote provider has no endpoint
- **WHEN** пользователь впервые выбирает `Ollama Remote` без сохранённого endpoint
- **THEN** приложение предлагает указать адрес сервиса и не отправляет модельный запрос до сохранения допустимого значения

#### Scenario: User saves Raspberry Pi endpoint
- **WHEN** пользователь вводит допустимый endpoint сервиса Raspberry Pi
- **THEN** приложение сохраняет endpoint только для `Ollama Remote` и использует его после перезапуска

#### Scenario: User enters an invalid endpoint
- **WHEN** пользователь вводит URL без допустимой HTTP(S) схемы или host либо URL с credentials, query или fragment
- **THEN** приложение сообщает об ошибке по-русски, не отправляет запрос и сохраняет предыдущее корректное значение

### Requirement: Remote models can be selected independently
Система SHALL получать список моделей с endpoint `Ollama Remote`, SHALL использовать `qwen3:1.7b` как модель этого провайдера по умолчанию и SHALL сохранять выбранную удалённую модель независимо от модели локального `Ollama`.

#### Scenario: Raspberry Pi exposes installed models
- **WHEN** endpoint `Ollama Remote` возвращает список установленных моделей
- **THEN** CLI показывает уникальные отсортированные имена и сохраняет выбранную модель только в конфигурации `Ollama Remote`

#### Scenario: Raspberry Pi has no installed models
- **WHEN** удалённый Ollama доступен, но не возвращает ни одной установленной модели
- **THEN** приложение сообщает по-русски, что на удалённом сервере необходимо установить модель, и не изменяет текущий выбор

### Requirement: Remote generation preserves agent behavior
Система SHALL направлять запросы `Ollama Remote` на сохранённый endpoint и SHALL передавать системные инструкции, выбранную модель, температуру, релевантную историю и допустимые параметры текущего запроса по тем же правилам, что и для локального `Ollama`. Ответ SHALL преобразовываться в общий результат агента с текстом, доступной token usage и structured tool calls.

#### Scenario: Remote model answers a chat request
- **WHEN** пользователь отправляет сообщение при активном `Ollama Remote`
- **THEN** приложение получает ответ с настроенного endpoint, показывает его и сохраняет сообщения в текущей сессии

#### Scenario: Conversation contains memory and mode instructions
- **WHEN** при активном `Ollama Remote` используются профиль, память, задача или режим ответа
- **THEN** удалённая модель получает тот же сформированный агентом контекст, который полагается выбранной стратегии

### Requirement: Remote failures are distinct and safe
Система MUST различать недоступность endpoint `Ollama Remote`, отсутствие выбранной модели и несовместимый Responses API и MUST возвращать понятную русскую ошибку без переключения на локальный `Ollama` или облачного провайдера.

#### Scenario: Raspberry Pi endpoint is unavailable
- **WHEN** соединение с настроенным endpoint не устанавливается
- **THEN** приложение сообщает, что удалённый Ollama недоступен, сохраняет текущую сессию и не выполняет fallback на другой провайдер

#### Scenario: Remote model is not installed
- **WHEN** удалённый Ollama сообщает, что выбранная модель отсутствует
- **THEN** приложение предлагает установить её на удалённом сервере или выбрать доступную модель и не выдаёт ложный успешный ответ

### Requirement: Remote provider identity survives persistence
Система SHALL сохранять и восстанавливать `Ollama Remote` как отдельную от локального `Ollama` идентичность в конфигурации, сессиях и метриках.

#### Scenario: Remote conversation is reopened
- **WHEN** пользователь сохраняет сессию с `Ollama Remote` и позднее загружает её
- **THEN** сессия восстанавливает удалённого провайдера, его модель и сообщения без подмены на локальный `Ollama`
