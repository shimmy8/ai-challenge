use crate::{memory::ActiveMemory, sessions::Message};

pub(crate) const MAX_RECENT_USER_MESSAGES: usize = 4;
pub(crate) const MAX_RECENT_USER_CHARS: usize = 2000;
pub(crate) const MAX_TASK_CONTEXT_CHARS: usize = 2000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RetrievalContext {
    pub(crate) recent_user_messages: Vec<String>,
    pub(crate) task_title: Option<String>,
    pub(crate) task_facts: Vec<String>,
}

impl RetrievalContext {
    pub(crate) fn from_state(history: &[Message], memory: &ActiveMemory) -> Self {
        let mut recent_reversed = Vec::new();
        let mut recent_chars = 0;
        for message in history
            .iter()
            .rev()
            .filter(|message| message.role == "user")
            .take(MAX_RECENT_USER_MESSAGES)
        {
            let chars = message.content.chars().count();
            if recent_chars + chars > MAX_RECENT_USER_CHARS {
                break;
            }
            recent_chars += chars;
            recent_reversed.push(message.content.clone());
        }
        recent_reversed.reverse();

        let mut task_title = None;
        let mut task_facts = Vec::new();
        if let Some(task) = &memory.task {
            let title_chars = task.title.chars().count();
            if title_chars <= MAX_TASK_CONTEXT_CHARS {
                task_title = Some(task.title.clone());
                let mut used = title_chars;
                for fact in &task.todo.facts {
                    let chars = fact.chars().count();
                    if used + chars > MAX_TASK_CONTEXT_CHARS {
                        break;
                    }
                    used += chars;
                    task_facts.push(fact.clone());
                }
            }
        }

        Self {
            recent_user_messages: recent_reversed,
            task_title,
            task_facts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryEntry, Profile, Task, TaskPhase, TaskTodo};

    fn message(role: &str, content: impl Into<String>) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
        }
    }

    #[test]
    fn context_keeps_four_recent_user_messages_in_chronological_order() {
        let history = vec![
            message("user", "один"),
            message("assistant", "секретный ответ"),
            message("user", "два"),
            message("user", "три"),
            message("user", "четыре"),
            message("user", "пять"),
        ];
        let context = RetrievalContext::from_state(&history, &ActiveMemory::default());
        assert_eq!(
            context.recent_user_messages,
            ["два", "три", "четыре", "пять"]
        );
        assert!(!format!("{context:?}").contains("секретный ответ"));
    }

    #[test]
    fn context_uses_whole_unicode_items_and_stops_at_budget() {
        let newest = "я".repeat(1000);
        let fits = "ё".repeat(1000);
        let excluded = "ю".to_owned();
        let history = vec![
            message("user", excluded.clone()),
            message("user", fits.clone()),
            message("user", newest.clone()),
        ];
        let context = RetrievalContext::from_state(&history, &ActiveMemory::default());
        assert_eq!(context.recent_user_messages, [fits, newest]);
        assert!(!context.recent_user_messages.contains(&excluded));
    }

    #[test]
    fn context_contains_only_task_title_and_budgeted_facts() {
        let first = "ц".repeat(900);
        let second = "т".repeat(900);
        let excluded = "ф".repeat(400);
        let memory = ActiveMemory {
            profile: Some(Profile {
                id: 9,
                name: "Секретный профиль".into(),
                instructions: "Не должно попасть".into(),
            }),
            task: Some(Task {
                id: 3,
                title: "Цель".into(),
                todo: TaskTodo {
                    facts: vec![first.clone(), second.clone(), excluded.clone()],
                    ..TaskTodo::default()
                },
                phase: TaskPhase::Planning,
                plan_version: 2,
                approved_plan_version: None,
                results: Vec::new(),
            }),
            long_term_facts: vec![MemoryEntry {
                id: 4,
                content: "Долговременный секрет".into(),
            }],
        };
        let context = RetrievalContext::from_state(&[], &memory);
        assert_eq!(context.task_title.as_deref(), Some("Цель"));
        assert_eq!(context.task_facts, [first, second]);
        let debug = format!("{context:?}");
        assert!(!debug.contains("профиль"));
        assert!(!debug.contains("Долговременный"));
        assert!(!debug.contains(&excluded));
    }
}
