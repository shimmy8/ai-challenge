## REMOVED Requirements

### Requirement: MCP configuration is backward-compatible
**Reason**: Учебный проект переходит на новый реестр серверов без поддержки одиночного legacy-формата.
**Migration**: Пользователь заново регистрирует нужные MCP-серверы через `/mcp`.

### Requirement: MCP menu reports connection state
**Reason**: Меню единственного сервера заменяется обзором и управлением несколькими серверами.
**Migration**: Состояние каждого endpoint показывается в общем списке `/mcp`.

### Requirement: New server address is validated before persistence
**Reason**: Операция смены единственного сервера заменяется независимым добавлением и редактированием записей реестра.
**Migration**: Каждый новый endpoint проходит handshake и `tools/list` перед сохранением.

### Requirement: User can inspect and select server tools
**Reason**: Allowlist с ручным включением заменяется автоматическим включением и явным denylist.
**Migration**: После регистрации доступны все инструменты, кроме отключённых пользователем.

### Requirement: Local demonstration server is independently runnable
**Reason**: Монолитный demo-сервер и инструмент `echo` удаляются в пользу предметных серверов.
**Migration**: Для проверки запускаются именованные локальные серверы `github`, `reporting`, `workspace`, `calendar`, `ai`, `telegram` и `orchestrator`.

## ADDED Requirements

### Requirement: MCP configuration stores named servers
Система SHALL хранить список MCP-серверов, каждый с уникальным стабильным `id`, HTTP(S)-адресом и списком отключённых нативных инструментов. Server ID MUST состоять из безопасных для имени инструмента строчных ASCII-букв, цифр и подчёркиваний и MUST NOT содержать разделитель `__`.

#### Scenario: Several servers survive restart
- **WHEN** пользователь регистрирует несколько проверенных endpoint и перезапускает `fox-llm`
- **THEN** приложение восстанавливает их ID, адреса и denylist независимо друг от друга

#### Scenario: Duplicate server ID is proposed
- **WHEN** пользователь пытается сохранить второй сервер с уже существующим ID
- **THEN** система отклоняет запись и не заменяет существующий сервер неявно

### Requirement: MCP menu manages servers independently
Команда `/mcp` SHALL показывать все зарегистрированные серверы, их адреса, доступность и число доступных и отключённых инструментов. Пользователь SHALL иметь возможность добавить, проверить, изменить и удалить конкретный сервер, не меняя остальные записи.

#### Scenario: One server is unavailable
- **WHEN** `/mcp` проверяет реестр и один endpoint не отвечает
- **THEN** меню показывает ошибку только для этого сервера и продолжает отображать доступные серверы

#### Scenario: Invalid endpoint is proposed
- **WHEN** новый адрес не является HTTP(S) endpoint либо не проходит handshake и `tools/list`
- **THEN** система не сохраняет его и показывает безопасную русскоязычную ошибку

### Requirement: Discovered tools are enabled by default
После успешного `tools/list` система SHALL считать доступным каждый инструмент, имя которого отсутствует в denylist данного сервера. Пользователь SHALL иметь возможность атомарно изменить denylist; новый инструмент сервера MUST становиться доступным автоматически.

#### Scenario: Server adds a new tool
- **WHEN** зарегистрированный сервер начинает объявлять ранее неизвестный инструмент
- **THEN** инструмент доступен агенту при следующем подключении, если пользователь явно его не отключил

#### Scenario: User disables a tool
- **WHEN** пользователь подтверждает отключение обнаруженного инструмента
- **THEN** его нативное имя сохраняется в denylist этого сервера и инструмент не публикуется модели

### Requirement: Local capabilities run as separate named servers
Репозиторий SHALL предоставлять отдельные режимы запуска серверов `github`, `reporting`, `workspace`, `calendar`, `ai`, `telegram` и `orchestrator` на явно заданных loopback-адресах. Серверы MUST объявлять только инструменты своей группы; инструмент `echo` MUST отсутствовать.

#### Scenario: Reporting server is discovered
- **WHEN** клиент подключается к запущенному серверу `reporting`
- **THEN** `tools/list` содержит инструменты расчёта и рендеринга отчёта и не содержит GitHub, календарных, файловых или scheduler-инструментов

#### Scenario: Echo is requested
- **WHEN** клиент ищет или вызывает инструмент `echo` на любом локальном сервере
- **THEN** инструмент отсутствует и вызов не выполняется
