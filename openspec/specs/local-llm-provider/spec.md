# local-llm-provider Specification

## Purpose

Позволяет пользователю выбирать и использовать локальную Ollama-модель в `fox-llm` без облачного API-ключа, сохраняя привычный CLI, историю, память и метрики агента.

## Requirements

### Requirement: Ollama is available as a local provider
Система SHALL предлагать Ollama наряду с OpenAI и Claude при выборе LLM-провайдера. Для локального провайдера система MUST NOT запрашивать, сохранять или отправлять API-ключ.

#### Scenario: User selects Ollama on first launch
- **WHEN** пользователь выбирает Ollama в списке провайдеров
- **THEN** приложение продолжает настройку без экрана ввода API-ключа и использует локальный endpoint

#### Scenario: User switches from a cloud provider to Ollama
- **WHEN** пользователь переключает активный провайдер с OpenAI или Claude на Ollama
- **THEN** агент перенастраивается на локальную модель без изменения или удаления сохранённых ключей облачных провайдеров

### Requirement: Local endpoint and default model are deterministic
Система SHALL обращаться к Ollama по локальному endpoint `http://127.0.0.1:11434/v1` и SHALL использовать `qwen3.5:4b` как модель по умолчанию, пока пользователь не выберет другую установленную модель.

#### Scenario: Existing configuration has no Ollama entry
- **WHEN** приложение загружает действующую конфигурацию, созданную до появления поддержки Ollama
- **THEN** оно добавляет настройки Ollama с локальным endpoint и моделью по умолчанию, сохраняя все существующие настройки без изменений

#### Scenario: User selected another local model
- **WHEN** пользователь сохранил другую установленную Ollama-модель и перезапустил приложение
- **THEN** приложение восстанавливает Ollama и выбранную модель как активные настройки

### Requirement: Installed Ollama models can be selected
Система SHALL получать список моделей, доступных локальному Ollama-серверу, показывать их через существующий интерактивный выбор модели и сохранять выбранное имя без фильтрации по правилам имён OpenAI.

#### Scenario: Local server exposes several models
- **WHEN** Ollama возвращает несколько установленных моделей
- **THEN** CLI показывает уникальные отсортированные имена всех возвращённых моделей и позволяет выбрать одну из них

#### Scenario: No local models are installed
- **WHEN** Ollama доступна, но не возвращает ни одной установленной модели
- **THEN** приложение сообщает по-русски, что сначала необходимо установить модель, и не изменяет текущую настройку

### Requirement: Local generation preserves agent behavior
Система SHALL передавать Ollama системные инструкции, выбранную модель, температуру, релевантную историю и допустимые параметры текущего запроса и SHALL преобразовывать ответ в общий результат агента с текстом, token usage и structured tool calls. История, память, режим ответа, сжатие контекста и метрики MUST работать по тем же правилам, что и для облачных провайдеров.

#### Scenario: Model answers a plain text request
- **WHEN** пользователь отправляет запрос при активном Ollama и локальная модель возвращает текст
- **THEN** приложение показывает ответ, сохраняет пользовательское сообщение и итоговый ответ и учитывает доступную статистику токенов

#### Scenario: Conversation contains memory and mode instructions
- **WHEN** активны профиль, долговременная память, задача или режим ответа
- **THEN** запрос к Ollama получает тот же сформированный агентом контекст, который полагается выбранной стратегии, без provider-specific потери данных

### Requirement: Local failures are actionable and safe
Система MUST различать как минимум недоступный Ollama-сервер, отсутствие выбранной модели и несовместимый Responses API и MUST возвращать понятную русскую ошибку без аварийного завершения и без ложного ответа модели.

#### Scenario: Ollama is not running
- **WHEN** соединение с локальным endpoint не устанавливается
- **THEN** приложение сообщает, что Ollama недоступна и её необходимо запустить, сохраняя текущую сессию

#### Scenario: Configured model is not installed
- **WHEN** Ollama сообщает, что выбранная модель отсутствует
- **THEN** приложение предлагает установить или выбрать доступную модель и не выдаёт результат как успешный

#### Scenario: Ollama does not support Responses API
- **WHEN** локальный сервер не поддерживает необходимый совместимый endpoint
- **THEN** приложение сообщает о требовании обновить Ollama до совместимой версии и не переключается незаметно на облачный провайдер

### Requirement: Provider identity survives session persistence
Система SHALL сохранять и восстанавливать Ollama как отдельную идентичность провайдера в конфигурации, сессиях и связанных метаданных.

#### Scenario: Ollama conversation is reopened
- **WHEN** пользователь сохраняет сессию с Ollama и позднее загружает её
- **THEN** сессия распознаётся как созданная провайдером Ollama и её сообщения восстанавливаются без ошибки десериализации

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
