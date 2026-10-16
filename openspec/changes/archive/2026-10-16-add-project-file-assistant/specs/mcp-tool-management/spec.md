## ADDED Requirements

### Requirement: Интерактивное приложение предоставляет изолированный project-only MCP
При запуске интерактивного режима система SHALL автоматически запускать отдельный внутренний loopback MCP для project workflow на свободном локальном порту. Сервер SHALL публиковать только инструменты списка файлов, текстового поиска, чтения файла, записи файла, Git diff и CodeGraph-анализа, MUST NOT требовать записи в пользовательский MCP registry и MUST NOT публиковать пользовательские, help-only, календарные, сетевые или произвольные shell-инструменты.

#### Scenario: Project tools are available without manual registration
- **WHEN** пользователь запускает `/project <цель>` в интерактивном `fox-llm`
- **THEN** свежий project-субагент получает квалифицированные project-only tools без отдельного процесса, фиксированного порта или настройки через `/mcp`

#### Scenario: Project tool set is isolated
- **WHEN** project-субагент получает `tools/list` внутреннего project MCP
- **THEN** список содержит только разрешённые project tools с корректными read-only аннотациями и не содержит инструменты пользовательского registry или help-only runtime

#### Scenario: Internal project MCP is unavailable
- **WHEN** project-only MCP невозможно запустить или подключить
- **THEN** обычный диалог и `/help` продолжают работу, а `/project` завершается до provider-вызова с явной диагностикой недоступности файлового workflow

#### Scenario: Application shuts down
- **WHEN** интерактивное приложение завершает работу
- **THEN** система закрывает project MCP client, ограничивает graceful shutdown тем же бюджетом, что и help-only runtime, после тайм-аута отменяет server task и не сохраняет запись в конфигурации

### Requirement: Project file tools are confined to the workspace
Все project file tools SHALL разрешать только нормализованные относительные пути внутри зафиксированного канонического корня workspace. Система MUST работать только с обычными UTF-8 файлами, MUST NOT следовать по символической ссылке, пересекающей файловую границу, и MUST исключать `.git`, `target`, локальные env-файлы, `.fox-llm.json`, `.fox-embeddings.json`, локальные SQLite-файлы, runtime-логи и иные явно запрещённые credentials или generated data. Gitignore-aware обход MAY дополнительно скрывать проигнорированные файлы, но MUST NOT ослаблять явный denylist.

#### Scenario: Relative project file is read
- **WHEN** модель запрашивает доступный обычный UTF-8 файл по относительному пути внутри workspace
- **THEN** инструмент возвращает ограниченное содержимое, относительный путь, диапазон строк, размер и SHA-256 без изменения файла

#### Scenario: Secret file is requested directly
- **WHEN** модель передаёт точное имя локального env-файла или другого denylisted файла
- **THEN** инструмент отклоняет запрос до чтения и возвращает безопасную ошибку без подтверждения существования или содержимого цели

#### Scenario: Path escapes through parent or symlink
- **WHEN** путь абсолютный, содержит выход через `..` либо разрешается через symlink за пределы workspace
- **THEN** инструмент отклоняет операцию до чтения или записи внешней цели

#### Scenario: Binary or oversized file is requested
- **WHEN** цель не является UTF-8 или превышает установленный лимит одного файла
- **THEN** инструмент возвращает ограниченную диагностическую ошибку и не передаёт содержимое модели

### Requirement: Project discovery is bounded and deterministic
Инструменты списка и поиска SHALL обходить доступные project files в стабильном порядке, применять предел числа файлов, прочитанных байтов, совпадений и размера ответа и возвращать признак усечения. Текстовый поиск SHALL быть literal UTF-8 search с относительными путями и однобазными номерами строк; он MUST NOT интерпретировать запрос как shell-команду или путь к исполняемому файлу. Git diff и CodeGraph SHALL запускаться без shell-интерпретации относительно фиксированного workspace и также применять тайм-аут и предел вывода.

#### Scenario: Search spans several files
- **WHEN** допустимая строка встречается в нескольких доступных файлах
- **THEN** поиск возвращает совпадения в стабильном порядке с относительным путём, однобазным номером строки и ограниченным фрагментом

#### Scenario: Search reaches a resource limit
- **WHEN** обход достигает предела файлов, байтов, совпадений, времени или размера ответа
- **THEN** инструмент прекращает дальнейший сбор, сохраняет уже безопасно собранные результаты и явно возвращает `truncated=true` с причиной

#### Scenario: Git diff is requested
- **WHEN** project-субагент запрашивает diff после записи
- **THEN** инструмент возвращает ограниченный diff только для workspace и не включает denylisted или untracked secret files
