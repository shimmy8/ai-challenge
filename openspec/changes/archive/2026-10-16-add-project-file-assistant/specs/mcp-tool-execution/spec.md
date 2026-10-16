## ADDED Requirements

### Requirement: Project file writes require a validated diff preview
Перед каждым project tool call записи система MUST валидировать режим `create` или `replace`, нормализовать относительный путь, проверить workspace policy и построить unified diff по текущему состоянию цели и точному предложенному UTF-8 содержимому. Подтверждение SHALL показывать режим, путь, old/new SHA-256, размер и ограниченный diff; если полный diff усечён, система MUST явно отметить усечение до вопроса пользователю. Ошибка подготовки preview MUST отклонять вызов без запроса общего подтверждения по сырым аргументам.

#### Scenario: Existing file preview is approved
- **WHEN** модель предлагает `replace` с hash текущей прочитанной версии и preview успешно построен
- **THEN** пользователь видит нормализованный путь и diff точного содержимого, которое будет записано

#### Scenario: New file preview is approved
- **WHEN** модель предлагает `create` для отсутствующего допустимого пути и preview успешно построен
- **THEN** пользователь видит diff создания из `/dev/null` до полного предложенного файла перед подтверждением

#### Scenario: Preview exceeds display limit
- **WHEN** корректный diff превышает предел интерактивного вывода
- **THEN** система показывает ограниченный diff, величину или признак усечения и не скрывает факт неполного preview

#### Scenario: Invalid write arguments are proposed
- **WHEN** режим, путь, hash либо содержимое не проходит валидацию
- **THEN** система не показывает общий JSON как замену diff preview, не спрашивает подтверждение и не выполняет запись

### Requirement: Confirmed file write is bound to the previewed state
После подтверждения система MUST повторно проверить путь, policy, режим и precondition непосредственно перед записью и выполнить ровно подтверждённые аргументы. `replace` MUST совпасть с подтверждённым old SHA-256, а `create` MUST обнаружить отсутствующую цель. При несовпадении система SHALL завершить tool call структурированной stale/conflict ошибкой без изменения цели и без автоматического повторного подтверждения.

#### Scenario: Confirmed state is unchanged
- **WHEN** path policy и precondition совпадают с показанным preview
- **THEN** система выполняет ровно подтверждённую запись и возвращает фактический diff и hashes

#### Scenario: File changes while confirmation is pending
- **WHEN** файл изменён другим процессом после preview и до выполнения
- **THEN** система не перезаписывает файл, возвращает conflict и требует нового чтения, нового diff и отдельного подтверждения для следующей попытки

#### Scenario: User rejects the preview
- **WHEN** пользователь отвечает «нет» на подтверждение project write
- **THEN** MCP write не вызывается, модели возвращается структурированная отмена, а содержимое и metadata цели не меняются
