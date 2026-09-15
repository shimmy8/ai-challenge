#![allow(unused_imports)]
use crate::{agent::Agent, cli::parse_compression_strategy, config::*, memory::*, model::*};
use anyhow::{anyhow, bail, Context, Result};
use console::{style, Key, Term};
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, Select};
use reqwest::{Client, StatusCode};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Message {
    pub(crate) role: String,
    pub(crate) content: String,
}

pub(crate) struct SavedSession {
    pub(crate) id: i64,
    pub(crate) title: String,
    pub(crate) provider: Provider,
    pub(crate) model: String,
    pub(crate) mode: Option<String>,
    pub(crate) temperature: f64,
    pub(crate) messages: Vec<Message>,
    pub(crate) summary: String,
    pub(crate) facts: BTreeMap<String, String>,
    pub(crate) summarized_count: usize,
    pub(crate) compression_strategy: CompressionStrategy,
    pub(crate) context_messages: usize,
    pub(crate) branches: HashMap<String, BranchState>,
    pub(crate) checkpoint: Option<BranchState>,
    pub(crate) active_branch: String,
    pub(crate) branch_pending: bool,
    pub(crate) profile: Option<Profile>,
    pub(crate) task: Option<Task>,
}

pub(crate) struct SessionStore {
    pub(crate) connection: Connection,
}

pub(crate) struct SessionSnapshot<'a> {
    pub(crate) provider: Provider,
    pub(crate) model: &'a str,
    pub(crate) mode: Option<&'a str>,
    pub(crate) temperature: f64,
    pub(crate) messages: &'a [Message],
    pub(crate) summary: &'a str,
    pub(crate) facts: &'a BTreeMap<String, String>,
    pub(crate) summarized_count: usize,
    pub(crate) compression_strategy: CompressionStrategy,
    pub(crate) context_messages: usize,
    pub(crate) branches: &'a HashMap<String, BranchState>,
    pub(crate) checkpoint: Option<&'a BranchState>,
    pub(crate) active_branch: &'a str,
    pub(crate) branch_pending: bool,
    pub(crate) profile_id: Option<i64>,
    pub(crate) task_id: Option<i64>,
}

impl SessionStore {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("не удалось открыть базу сессий {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
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
             CREATE TABLE IF NOT EXISTS tasks (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 title TEXT NOT NULL,
                 todo TEXT NOT NULL,
                 phase TEXT NOT NULL DEFAULT 'planning',
                 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                 updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
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
                "profile_id",
                "profile_id INTEGER REFERENCES profiles(id) ON DELETE SET NULL",
            ),
            (
                "task_id",
                "task_id INTEGER REFERENCES tasks(id) ON DELETE SET NULL",
            ),
        ] {
            if !has_column(&connection, "sessions", column)? {
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
        Ok(Self { connection })
    }

    pub(crate) fn save(&self, id: Option<i64>, snapshot: SessionSnapshot<'_>) -> Result<i64> {
        let transaction = self.connection.unchecked_transaction()?;
        let history = encode_messages_toon(snapshot.messages);
        let title = snapshot
            .messages
            .iter()
            .find(|message| message.role == "user")
            .map(|message| session_title(&message.content))
            .unwrap_or_else(|| "Новая сессия".to_owned());
        let provider = provider_id(snapshot.provider);
        let saved_id = if let Some(id) = id {
            self.connection.execute(
                "UPDATE sessions SET title = ?1, provider = ?2, model = ?3, mode = ?4,
                 temperature = ?5, history_toon = ?6, profile_id = ?7, task_id = ?8,
                 updated_at = CURRENT_TIMESTAMP WHERE id = ?9",
                params![
                    title,
                    provider,
                    snapshot.model,
                    snapshot.mode,
                    snapshot.temperature,
                    history,
                    snapshot.profile_id,
                    snapshot.task_id,
                    id
                ],
            )?;
            id
        } else {
            self.connection.execute(
                "INSERT INTO sessions
                 (title, provider, model, mode, temperature, history_toon, profile_id, task_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    title,
                    provider,
                    snapshot.model,
                    snapshot.mode,
                    snapshot.temperature,
                    history,
                    snapshot.profile_id,
                    snapshot.task_id
                ],
            )?;
            self.connection.last_insert_rowid()
        };
        let facts_json = serde_json::to_string(snapshot.facts)?;
        self.connection.execute(
            "INSERT OR REPLACE INTO session_context
             (session_id, summary, summarized_count, facts_json, compression_strategy,
              context_messages, branch_pending)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                saved_id,
                snapshot.summary,
                snapshot.summarized_count,
                facts_json,
                snapshot.compression_strategy.to_string(),
                snapshot.context_messages,
                snapshot.branch_pending
            ],
        )?;
        self.replace_branching_state(
            saved_id,
            snapshot.branches,
            snapshot.checkpoint,
            snapshot.active_branch,
        )?;
        transaction.commit()?;
        Ok(saved_id)
    }

    pub(crate) fn list(&self) -> Result<Vec<(i64, String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT id, title, updated_at FROM sessions ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn load(&self, id: i64) -> Result<SavedSession> {
        let row = self.connection.query_row(
            "SELECT id, title, provider, model, mode, temperature, history_toon,
                    profile_id, task_id
             FROM sessions WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            },
        )?;
        let (
            summary,
            summarized_count,
            facts_json,
            compression_strategy,
            context_messages,
            branch_pending,
        ) = self
            .connection
            .query_row(
                "SELECT summary, summarized_count, facts_json, compression_strategy,
                        context_messages, branch_pending
                 FROM session_context WHERE session_id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, usize>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, usize>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()?
            .unwrap_or_else(|| {
                (
                    String::new(),
                    0,
                    "{}".to_owned(),
                    CompressionStrategy::Summary.to_string(),
                    default_context_messages(),
                    false,
                )
            });
        let facts = serde_json::from_str(&facts_json).context("повреждены facts сессии")?;
        let compression_strategy = parse_compression_strategy(&compression_strategy)
            .context("повреждена стратегия контекста сессии")?;
        anyhow::ensure!(context_messages <= 1000, "повреждён размер окна сессии");
        let messages = decode_messages_toon(&row.6)?;
        let profile = row.7.map(|id| self.load_profile(id)).transpose()?;
        let task = row.8.map(|id| self.load_task(id)).transpose()?;
        anyhow::ensure!(
            summarized_count <= messages.len(),
            "повреждён контекст сессии"
        );
        let mut branches = HashMap::new();
        let mut active_branch = "main".to_owned();
        {
            let mut statement = self.connection.prepare(
                "SELECT name, history_toon, summary, facts_json, is_active
                 FROM session_branches WHERE session_id = ?1 ORDER BY name",
            )?;
            let rows = statement.query_map([id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })?;
            for row in rows {
                let (name, history_toon, summary, facts_json, is_active) = row?;
                let persisted_history = decode_messages_toon(&history_toon)?;
                let facts =
                    serde_json::from_str(&facts_json).context("повреждены facts ветки сессии")?;
                if is_active {
                    active_branch = name.clone();
                }
                branches.insert(
                    name,
                    BranchState {
                        history: persisted_history.clone(),
                        persisted_history,
                        summary,
                        facts,
                    },
                );
            }
        }
        let checkpoint = self
            .connection
            .query_row(
                "SELECT history_toon, summary, facts_json
                 FROM session_checkpoints WHERE session_id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(history_toon, summary, facts_json)| -> Result<BranchState> {
                    let persisted_history = decode_messages_toon(&history_toon)?;
                    Ok(BranchState {
                        history: persisted_history.clone(),
                        persisted_history,
                        summary,
                        facts: serde_json::from_str(&facts_json)
                            .context("повреждены facts checkpoint сессии")?,
                    })
                },
            )
            .transpose()?;
        Ok(SavedSession {
            id: row.0,
            title: row.1,
            provider: parse_provider(&row.2)?,
            model: row.3,
            mode: row.4,
            temperature: row.5,
            messages,
            summary,
            facts,
            summarized_count,
            compression_strategy,
            context_messages,
            branches,
            checkpoint,
            active_branch,
            branch_pending,
            profile,
            task,
        })
    }

    pub(crate) fn create_profile(&self, name: &str, instructions: &str) -> Result<Profile> {
        let (name, instructions) = validate_profile(name, instructions)?;
        self.connection
            .execute(
                "INSERT INTO profiles (name, instructions) VALUES (?1, ?2)",
                params![name, instructions],
            )
            .with_context(|| format!("не удалось создать профиль «{name}»"))?;
        self.load_profile(self.connection.last_insert_rowid())
    }

    pub(crate) fn create_memory_entry(&self, content: &str) -> Result<MemoryEntry> {
        let content = validate_memory_entry(content)?;
        self.connection
            .execute(
                "INSERT INTO memory_entries (content) VALUES (?1)",
                [content.as_str()],
            )
            .context("не удалось сохранить долговременный факт")?;
        self.load_memory_entry(self.connection.last_insert_rowid())
    }

    pub(crate) fn list_memory_entries(&self) -> Result<Vec<MemoryEntry>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, content FROM memory_entries ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok(MemoryEntry {
                id: row.get(0)?,
                content: row.get(1)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn load_memory_entry(&self, id: i64) -> Result<MemoryEntry> {
        self.connection
            .query_row(
                "SELECT id, content FROM memory_entries WHERE id = ?1",
                [id],
                |row| {
                    Ok(MemoryEntry {
                        id: row.get(0)?,
                        content: row.get(1)?,
                    })
                },
            )
            .with_context(|| format!("долговременный факт #{id} не найден"))
    }

    pub(crate) fn delete_memory_entry(&self, id: i64) -> Result<()> {
        let deleted = self
            .connection
            .execute("DELETE FROM memory_entries WHERE id = ?1", [id])?;
        anyhow::ensure!(deleted == 1, "долговременный факт #{id} не найден");
        Ok(())
    }

    pub(crate) fn list_profiles(&self) -> Result<Vec<Profile>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, name, instructions FROM profiles ORDER BY name, id")?;
        let rows = statement.query_map([], |row| {
            Ok(Profile {
                id: row.get(0)?,
                name: row.get(1)?,
                instructions: row.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn load_profile(&self, id: i64) -> Result<Profile> {
        self.connection
            .query_row(
                "SELECT id, name, instructions FROM profiles WHERE id = ?1",
                [id],
                |row| {
                    Ok(Profile {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        instructions: row.get(2)?,
                    })
                },
            )
            .with_context(|| format!("профиль #{id} не найден"))
    }

    pub(crate) fn create_task(&self, title: &str, todo: &str) -> Result<Task> {
        let (title, todo) = validate_task(title, todo)?;
        self.connection.execute(
            "INSERT INTO tasks (title, todo, phase) VALUES (?1, ?2, 'planning')",
            params![title, todo],
        )?;
        self.load_task(self.connection.last_insert_rowid())
    }

    pub(crate) fn list_tasks(&self) -> Result<Vec<Task>> {
        let mut statement = self.connection.prepare(
            "SELECT id, title, todo, phase FROM tasks ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (id, title, todo, phase) = row?;
            Ok(Task {
                id,
                title,
                todo,
                phase: phase.parse()?,
            })
        })
        .collect()
    }

    pub(crate) fn load_task(&self, id: i64) -> Result<Task> {
        let (id, title, todo, phase) = self
            .connection
            .query_row(
                "SELECT id, title, todo, phase FROM tasks WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .with_context(|| format!("задача #{id} не найдена"))?;
        Ok(Task {
            id,
            title,
            todo,
            phase: phase.parse()?,
        })
    }

    pub(crate) fn update_task_todo(&self, id: i64, todo: &str) -> Result<Task> {
        let todo = validate_memory_text(todo, "TODO задачи", TASK_TODO_MAX_CHARS)?;
        let updated = self.connection.execute(
            "UPDATE tasks SET todo = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
            params![todo, id],
        )?;
        anyhow::ensure!(updated == 1, "задача #{id} не найдена");
        self.load_task(id)
    }

    pub(crate) fn advance_task(&self, id: i64) -> Result<Task> {
        let transaction = self.connection.unchecked_transaction()?;
        let task = self.load_task(id)?;
        let next = task
            .phase
            .next()
            .with_context(|| format!("задача «{}» уже завершена", task.title))?;
        let updated = self.connection.execute(
            "UPDATE tasks SET phase = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?2 AND phase = ?3",
            params![next.to_string(), id, task.phase.to_string()],
        )?;
        anyhow::ensure!(updated == 1, "фаза задачи изменилась; повторите команду");
        transaction.commit()?;
        self.load_task(id)
    }

    pub(crate) fn replace_branching_state(
        &self,
        session_id: i64,
        branches: &HashMap<String, BranchState>,
        checkpoint: Option<&BranchState>,
        active_branch: &str,
    ) -> Result<()> {
        self.connection.execute(
            "DELETE FROM session_branches WHERE session_id = ?1",
            [session_id],
        )?;
        for (name, state) in branches {
            self.connection.execute(
                "INSERT INTO session_branches
                 (session_id, name, history_toon, summary, facts_json, is_active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    session_id,
                    name,
                    encode_messages_toon(&state.persisted_history),
                    state.summary,
                    serde_json::to_string(&state.facts)?,
                    name == active_branch
                ],
            )?;
        }
        if let Some(state) = checkpoint {
            self.connection.execute(
                "INSERT OR REPLACE INTO session_checkpoints
                 (session_id, history_toon, summary, facts_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    session_id,
                    encode_messages_toon(&state.persisted_history),
                    state.summary,
                    serde_json::to_string(&state.facts)?
                ],
            )?;
        } else {
            self.connection.execute(
                "DELETE FROM session_checkpoints WHERE session_id = ?1",
                [session_id],
            )?;
        }
        Ok(())
    }

    pub(crate) fn save_branching_state(&self, session_id: i64, agent: &Agent) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        let state = agent.snapshot();
        self.connection.execute(
            "UPDATE sessions SET history_toon = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
            params![encode_messages_toon(&state.persisted_history), session_id],
        )?;
        self.connection.execute(
            "UPDATE session_context
             SET summary = ?1, summarized_count = ?2, facts_json = ?3, branch_pending = ?4
             WHERE session_id = ?5",
            params![
                state.summary,
                state
                    .persisted_history
                    .len()
                    .saturating_sub(state.history.len()),
                serde_json::to_string(&state.facts)?,
                agent.branch_pending,
                session_id
            ],
        )?;
        self.replace_branching_state(
            session_id,
            &agent.branches,
            agent.checkpoint.as_ref(),
            &agent.active_branch,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn update_compression(
        &self,
        id: i64,
        strategy: CompressionStrategy,
        context_messages: usize,
    ) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE session_context
             SET compression_strategy = ?1, context_messages = ?2,
                 summary = '', summarized_count = 0
             WHERE session_id = ?3",
            params![strategy.to_string(), context_messages, id],
        )?;
        anyhow::ensure!(updated == 1, "контекст активной сессии не найден");
        Ok(())
    }

    pub(crate) fn delete(&self, id: i64) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?
            > 0)
    }
}

fn has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    Ok(columns
        .collect::<std::result::Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column))
}

pub(crate) fn provider_id(provider: Provider) -> &'static str {
    match provider {
        Provider::Openai => "openai",
        Provider::Claude => "claude",
    }
}

pub(crate) fn parse_provider(value: &str) -> Result<Provider> {
    match value {
        "openai" => Ok(Provider::Openai),
        "claude" => Ok(Provider::Claude),
        _ => bail!("неизвестный провайдер в сохранённой сессии: {value}"),
    }
}

pub(crate) fn session_title(content: &str) -> String {
    let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let title: String = chars.by_ref().take(48).collect();
    if chars.next().is_some() {
        format!("{title}…")
    } else {
        title
    }
}

pub(crate) fn encode_messages_toon(messages: &[Message]) -> String {
    let mut output = format!("messages[{}]{{role,content}}:", messages.len());
    for message in messages {
        let content = serde_json::to_string(&message.content).expect("String always serializes");
        output.push_str(&format!("\n  {},{}", message.role, content));
    }
    output
}

pub(crate) fn decode_messages_toon(input: &str) -> Result<Vec<Message>> {
    let mut lines = input.lines();
    let header = lines.next().context("пустая история TOON")?;
    if !header.starts_with("messages[") || !header.ends_with("{role,content}:") {
        bail!("неверный заголовок истории TOON");
    }
    let declared_count = header
        .strip_prefix("messages[")
        .and_then(|value| value.strip_suffix("]{role,content}:"))
        .context("неверный заголовок истории TOON")?
        .parse::<usize>()
        .context("неверное число сообщений в истории TOON")?;
    let messages = lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (role, content) = line
                .trim_start()
                .split_once(',')
                .context("повреждена строка истории TOON")?;
            if role != "user" && role != "assistant" {
                bail!("неизвестная роль в истории TOON: {role}");
            }
            Ok(Message {
                role: role.to_owned(),
                content: serde_json::from_str(content)
                    .context("повреждено содержимое сообщения TOON")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if messages.len() != declared_count {
        bail!(
            "число сообщений в истории TOON не совпадает: ожидалось {declared_count}, найдено {}",
            messages.len()
        );
    }
    Ok(messages)
}

pub(crate) fn parse_facts(input: &str) -> Result<BTreeMap<String, String>> {
    let trimmed = input.trim();
    let json_text = if trimmed.starts_with("```") {
        trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .and_then(|value| value.strip_suffix("```"))
            .map(str::trim)
            .context("модель вернула некорректный блок facts")?
    } else {
        trimmed
    };
    let facts: BTreeMap<String, String> = serde_json::from_str(json_text)
        .context("модель вернула facts не в формате JSON key-value")?;
    Ok(facts)
}
