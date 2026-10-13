use anyhow::Result;
use rusqlite::Connection;

pub(super) fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE IF NOT EXISTS profiles (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             name TEXT NOT NULL UNIQUE,
             instructions TEXT NOT NULL,
             created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
             updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS memory_entries (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             content TEXT NOT NULL,
             created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS invariants (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             content TEXT NOT NULL,
             enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
             created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS tasks (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             title TEXT NOT NULL,
             todo TEXT NOT NULL,
             phase TEXT NOT NULL DEFAULT 'planning',
             plan_version INTEGER NOT NULL DEFAULT 0 CHECK (plan_version >= 0),
             approved_plan_version INTEGER CHECK (approved_plan_version IS NULL OR approved_plan_version >= 0),
             created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
             updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS task_step_results (
             task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
             phase TEXT NOT NULL CHECK (phase IN ('execution', 'validation')),
             item_id INTEGER NOT NULL,
             summary TEXT NOT NULL,
             content TEXT NOT NULL,
             PRIMARY KEY (task_id, phase, item_id)
         );
         CREATE TABLE IF NOT EXISTS sessions (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             title TEXT NOT NULL,
             provider TEXT NOT NULL,
             model TEXT NOT NULL,
             mode TEXT,
             temperature REAL NOT NULL,
             history_toon TEXT NOT NULL,
             profile_id INTEGER REFERENCES profiles(id) ON DELETE SET NULL,
             task_id INTEGER REFERENCES tasks(id) ON DELETE SET NULL,
             created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
             updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS session_context (
             session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
             summary TEXT NOT NULL,
             summarized_count INTEGER NOT NULL,
             facts_json TEXT NOT NULL DEFAULT '{}',
             compression_strategy TEXT NOT NULL DEFAULT 'summary',
             context_messages INTEGER NOT NULL DEFAULT 10,
             branch_pending INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS session_branches (
             session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
             name TEXT NOT NULL,
             history_toon TEXT NOT NULL,
             summary TEXT NOT NULL,
             facts_json TEXT NOT NULL,
             is_active INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (session_id, name)
         );
         CREATE TABLE IF NOT EXISTS session_checkpoints (
             session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
             history_toon TEXT NOT NULL,
             summary TEXT NOT NULL,
             facts_json TEXT NOT NULL
         );
         PRAGMA foreign_keys = ON;",
    )?;
    for (column, definition) in [
        (
            "plan_version",
            "plan_version INTEGER NOT NULL DEFAULT 0 CHECK (plan_version >= 0)",
        ),
        (
            "approved_plan_version",
            "approved_plan_version INTEGER CHECK (approved_plan_version IS NULL OR approved_plan_version >= 0)",
        ),
    ] {
        if !has_column(connection, "tasks", column)? {
            connection.execute(&format!("ALTER TABLE tasks ADD COLUMN {definition}"), [])?;
        }
    }
    if !has_column(connection, "invariants", "enabled")? {
        connection.execute(
            "ALTER TABLE invariants ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))",
            [],
        )?;
    }
    for (column, definition) in [
        (
            "profile_id",
            "profile_id INTEGER REFERENCES profiles(id) ON DELETE SET NULL",
        ),
        (
            "task_id",
            "task_id INTEGER REFERENCES tasks(id) ON DELETE SET NULL",
        ),
    ] {
        if !has_column(connection, "sessions", column)? {
            connection.execute(&format!("ALTER TABLE sessions ADD COLUMN {definition}"), [])?;
        }
    }
    let has_facts_column = {
        let mut statement = connection.prepare("PRAGMA table_info(session_context)")?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
        columns
            .collect::<std::result::Result<Vec<_>, _>>()?
            .iter()
            .any(|column| column == "facts_json")
    };
    if !has_facts_column {
        connection.execute(
            "ALTER TABLE session_context ADD COLUMN facts_json TEXT NOT NULL DEFAULT '{}'",
            [],
        )?;
    }
    for (column, definition) in [
        (
            "compression_strategy",
            "compression_strategy TEXT NOT NULL DEFAULT 'summary'",
        ),
        (
            "context_messages",
            "context_messages INTEGER NOT NULL DEFAULT 10",
        ),
        (
            "branch_pending",
            "branch_pending INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        let exists = {
            let mut statement = connection.prepare("PRAGMA table_info(session_context)")?;
            let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
            columns
                .collect::<std::result::Result<Vec<_>, _>>()?
                .iter()
                .any(|name| name == column)
        };
        if !exists {
            connection.execute(
                &format!("ALTER TABLE session_context ADD COLUMN {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    Ok(columns
        .collect::<std::result::Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column))
}
