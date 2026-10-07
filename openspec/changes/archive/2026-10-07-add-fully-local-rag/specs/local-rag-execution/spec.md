## Purpose

Обеспечивает работу существующего RAG-пайплайна и индекса Недели 6 с локальной Ollama-моделью для query rewrite, grounded generation и repair без изменения embedding-пространства.

## ADDED Requirements

### Requirement: RAG использует неизменный индекс Недели 6
При включённом `/rag on` система SHALL читать опубликованные structural-чанки и embeddings из существующего `.fox-index.db` и SHALL использовать текущую `.fox-embeddings.json` для embedding исходного и переписанного запроса. Реализация Дня 28 MUST NOT требовать переиндексации, другой embedding-модели, отдельного index path или изменения SQLite-схемы.

#### Scenario: Индекс совместим с embedding-конфигурацией
- **WHEN** `.fox-index.db` содержит structural-чанки с model и dimensions, совпадающими с текущей `.fox-embeddings.json`
- **THEN** RAG использует этот индекс без его перезаписи и без создания нового embedding-пространства

#### Scenario: Индекс несовместим
- **WHEN** model или dimensions embedding запроса не совпадают с опубликованными vectors
- **THEN** система сохраняет существующую ошибку совместимости и не пытается автоматически переиндексировать корпус

### Requirement: Ollama выполняет модельные этапы RAG
Когда активным generation provider выбрана Ollama, система SHALL выполнять через выбранную локальную модель query rewrite, основной grounded generation request и единственный repair request, если первый draft нарушил контракт атрибуции. Эти этапы SHALL получать те же изолированные инструкции, исходный вопрос, retrieval-контекст и ограничения, что и при облачном provider.

#### Scenario: Успешный ответ без repair
- **WHEN** Ollama переписывает поисковый запрос, retrieval возвращает релевантные чанки и первый draft проходит строгую проверку атрибуции
- **THEN** пользователь получает проверенный grounded-ответ с источниками и цитатами без вызова облачной generation-модели

#### Scenario: Локальный repair
- **WHEN** первый draft Ollama нарушает grounded-response contract
- **THEN** система выполняет не более одного repair-запроса через ту же Ollama-модель и не переключается на OpenAI или Claude

#### Scenario: Ollama недоступна
- **WHEN** локальный Ollama server не принимает query rewrite либо generation request
- **THEN** текущий RAG-ход завершается понятной русской ошибкой без скрытого облачного fallback и без ложного ответа

### Requirement: Retrieval после embedding выполняется локально
Система SHALL загружать совместимые vectors из локального SQLite-индекса, вычислять cosine similarity, объединять исходный и переписанный запросы, применять порог, reranking, top-K и контекстный бюджет внутри процесса `fox-llm`. Retrieval MUST NOT передавать индекс или сохранённые чанки внешнему поисковому сервису.

#### Scenario: Успешный локальный поиск
- **WHEN** embedding endpoint вернул vectors вопроса и rewrite
- **THEN** все операции поиска, фильтрации и reranking выполняются над `.fox-index.db` локально и в generation prompt попадают только отобранные чанки

#### Scenario: Релевантный контекст отсутствует
- **WHEN** локальный поиск отсёк все candidates по существующему порогу
- **THEN** система возвращает детерминированный no-context ответ и не вызывает Ollama для generation

### Requirement: Grounded-контракт не ослабляется для локальной модели
Ответ Ollama SHALL проходить существующие проверки структуры attribution-блока, идентичности source metadata, дословности цитат и ссылок в Markdown. История SHALL сохранять исходный вопрос и очищенный проверенный ответ, но MUST NOT сохранять служебный prompt или полные retrieval-чанки.

#### Scenario: Неподтверждённая цитата
- **WHEN** Ollama после допустимого repair возвращает citation, которой нет в соответствующем retrieval-чанке
- **THEN** система отклоняет ответ и не показывает его как успешный

#### Scenario: Валидная атрибуция
- **WHEN** все source metadata и цитаты подтверждаются переданными чанками
- **THEN** CLI показывает очищенный ответ и отдельные человекочитаемые разделы источников и цитат

### Requirement: Граница локальности описывается точно
Отчёт Дня 28 SHALL отдельно указывать, что SQLite retrieval и Ollama generation выполняются локально, а query embeddings создаются существующим endpoint из `.fox-embeddings.json`. Отчёт MUST NOT называть систему полностью offline, если этот endpoint не локален.

#### Scenario: Используется OpenAI embedding endpoint
- **WHEN** эксперимент сохраняет `text-embedding-3-small` и внешний embedding endpoint Недели 6
- **THEN** отчёт явно отмечает единственный внешний этап и не приписывает ему локальное выполнение
