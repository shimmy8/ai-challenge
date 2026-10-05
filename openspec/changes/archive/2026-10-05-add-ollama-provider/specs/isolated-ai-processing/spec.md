## MODIFIED Requirements

### Requirement: Provider credentials and failures remain local
Учётные данные OpenAI или Claude MUST загружаться только локальным AI-сервером и MUST NOT появляться в схеме инструмента, аргументах, результате, trace, stdout или пользовательской ошибке. Изолированный запрос через локальный Ollama MUST выполняться без облачных credentials и MUST NOT отправляться OpenAI или Claude.

#### Scenario: Provider is unavailable
- **WHEN** модельный запрос завершается сетевой ошибкой, тайм-аутом или отказом провайдера
- **THEN** инструмент возвращает безопасную ошибку без учётных данных и без выдуманного результата

#### Scenario: Ollama is selected for isolated processing
- **WHEN** активным провайдером сохранена Ollama и `generate_text` выполняет изолированное преобразование
- **THEN** запрос направляется локальной Ollama-модели без API-ключа и результат не содержит сведения о credentials других провайдеров
