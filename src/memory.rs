use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

pub(crate) const MEMORY_NAME_MAX_CHARS: usize = 80;
pub(crate) const MEMORY_ENTRY_MAX_CHARS: usize = 1000;
pub(crate) const PROFILE_INSTRUCTIONS_MAX_CHARS: usize = 2000;
pub(crate) const TASK_TITLE_MAX_CHARS: usize = 120;
pub(crate) const TASK_TODO_MAX_CHARS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Profile {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) instructions: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryEntry {
    pub(crate) id: i64,
    pub(crate) content: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskPhase {
    Planning,
    Execution,
    Validation,
    Done,
}

impl TaskPhase {
    pub(crate) fn next(self) -> Option<Self> {
        match self {
            Self::Planning => Some(Self::Execution),
            Self::Execution => Some(Self::Validation),
            Self::Validation => Some(Self::Done),
            Self::Done => None,
        }
    }

    pub(crate) fn instructions(self) -> &'static str {
        match self {
            Self::Planning => {
                "Сейчас этап planning: уточняй план и TODO, не переходи к выполнению. Когда план готов, можешь предложить пользователю команду /task next."
            }
            Self::Execution => {
                "Сейчас этап execution: выполняй согласованный TODO. Когда работа завершена, можешь предложить пользователю команду /task next для проверки."
            }
            Self::Validation => {
                "Сейчас этап validation: проверяй результат и сообщай найденные проблемы. Когда проверка успешна, можешь предложить пользователю команду /task next."
            }
            Self::Done => {
                "Задача находится в терминальном этапе done. Следующего этапа нет; не предлагай менять фазу."
            }
        }
    }
}

impl fmt::Display for TaskPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Planning => "planning",
            Self::Execution => "execution",
            Self::Validation => "validation",
            Self::Done => "done",
        })
    }
}

impl FromStr for TaskPhase {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "planning" => Ok(Self::Planning),
            "execution" => Ok(Self::Execution),
            "validation" => Ok(Self::Validation),
            "done" => Ok(Self::Done),
            _ => bail!("неизвестная фаза задачи: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Task {
    pub(crate) id: i64,
    pub(crate) title: String,
    pub(crate) todo: String,
    pub(crate) phase: TaskPhase,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ActiveMemory {
    pub(crate) profile: Option<Profile>,
    pub(crate) task: Option<Task>,
    pub(crate) long_term_facts: Vec<MemoryEntry>,
}

pub(crate) fn validate_memory_text(value: &str, label: &str, max_chars: usize) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{label} не может быть пустым");
    }
    let length = value.chars().count();
    if length > max_chars {
        bail!("{label} слишком длинный: максимум {max_chars} символов, получено {length}");
    }
    Ok(value.to_owned())
}

pub(crate) fn validate_profile(name: &str, instructions: &str) -> Result<(String, String)> {
    Ok((
        validate_memory_text(name, "название профиля", MEMORY_NAME_MAX_CHARS)?,
        validate_memory_text(
            instructions,
            "инструкции профиля",
            PROFILE_INSTRUCTIONS_MAX_CHARS,
        )?,
    ))
}

pub(crate) fn validate_memory_entry(content: &str) -> Result<String> {
    validate_memory_text(content, "долговременный факт", MEMORY_ENTRY_MAX_CHARS)
}

pub(crate) fn compose_profile_instructions(
    response_style: &str,
    response_format: &str,
    constraints: &str,
) -> Result<String> {
    let response_style = validate_memory_text(
        response_style,
        "стиль ответа",
        PROFILE_INSTRUCTIONS_MAX_CHARS,
    )?;
    let response_format = validate_memory_text(
        response_format,
        "формат ответа",
        PROFILE_INSTRUCTIONS_MAX_CHARS,
    )?;
    let constraints = validate_memory_text(
        constraints,
        "ограничения ответа",
        PROFILE_INSTRUCTIONS_MAX_CHARS,
    )?;
    validate_memory_text(
        &format!(
            "Стиль:\n{response_style}\n\nФормат:\n{response_format}\n\nОграничения:\n{constraints}"
        ),
        "инструкции профиля",
        PROFILE_INSTRUCTIONS_MAX_CHARS,
    )
}

pub(crate) fn validate_task(title: &str, todo: &str) -> Result<(String, String)> {
    Ok((
        validate_memory_text(title, "название задачи", TASK_TITLE_MAX_CHARS)?,
        validate_memory_text(todo, "TODO задачи", TASK_TODO_MAX_CHARS)?,
    ))
}
