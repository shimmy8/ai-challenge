#![allow(unused_imports)]
use crate::*;
use reqwest::Client;
use rustyline::{
    completion::Completer, highlight::Highlighter, hint::Hinter, history::DefaultHistory,
    Context as ReadlineContext,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
#[cfg(test)]
mod suite {
    use super::*;
    use crate::app::{
        activate_memory_context, advance_task_if_confirmed, create_memory_entry_if_confirmed,
        create_profile_if_confirmed, create_task_if_confirmed, forget_memory_entry_if_confirmed,
        handle_profile_command, handle_remember_command, handle_task_command,
        update_task_todo_if_confirmed, ProfileCommandOutcome, TaskCommandOutcome,
    };

    #[test]
    pub(crate) fn parses_openai_response() {
        let body = json!({"output": [{"content": [{"type": "output_text", "text": "Привет!"}]}]});
        assert_eq!(extract_openai_text(&body).unwrap(), "Привет!");
    }

    #[test]
    pub(crate) fn parses_claude_response() {
        let body = json!({"content": [{"type": "text", "text": "Привет!"}]});
        assert_eq!(extract_claude_text(&body).unwrap(), "Привет!");
    }

    #[test]
    pub(crate) fn parses_dump_metrics_flag() {
        assert!(!parse_dump_metrics_flag(Vec::new()).unwrap());
        assert!(parse_dump_metrics_flag(vec!["--dump-metrics".into()]).unwrap());
        assert!(parse_dump_metrics_flag(vec!["--unknown".into()]).is_err());
    }

    #[test]
    pub(crate) fn appends_json_metrics_log() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metrics.log");
        let entry = MetricsLogEntry {
            timestamp_unix_ms: 123,
            session_id: 42,
            branch: "variant-a",
            agent_id: 1,
            provider: Provider::Openai,
            model: "gpt-test",
            elapsed_ms: 456,
            request_input_tokens: 10,
            request_output_tokens: 20,
            session_input_tokens: 30,
            session_output_tokens: 40,
        };

        append_metrics_log(&path, &entry).unwrap();
        append_metrics_log(&path, &entry).unwrap();

        let lines = fs::read_to_string(path).unwrap();
        assert_eq!(lines.lines().count(), 2);
        let value: Value = serde_json::from_str(lines.lines().next().unwrap()).unwrap();
        assert_eq!(value["session_id"], 42);
        assert_eq!(value["branch"], "variant-a");
        assert_eq!(value["request_input_tokens"], 10);
        assert_eq!(value["session_output_tokens"], 40);
    }

    #[test]
    pub(crate) fn config_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = Config {
            last_provider: Some(Provider::Claude),
            ..Config::default()
        };
        config.set_key(Provider::Claude, "secret".into());
        config
            .set_model(Provider::Claude, "claude-test".into())
            .unwrap();
        config.last_mode = Some("Кратко".into());
        config.set_temperature(Provider::Claude, 0.7).unwrap();
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.last_provider, Some(Provider::Claude));
        assert_eq!(loaded.key(Provider::Claude), Some("secret"));
        assert_eq!(loaded.model(Provider::Claude).unwrap(), "claude-test");
        assert_eq!(loaded.providers.len(), 2);
        assert_eq!(loaded.last_mode.as_deref(), Some("Кратко"));
        assert_eq!(loaded.temperature(Provider::Claude).unwrap(), 0.7);
        assert_eq!(loaded.temperature(Provider::Openai).unwrap(), 1.0);
    }

    #[test]
    pub(crate) fn modes_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("modes.json");
        let modes = ModesConfig {
            modes: vec![ResponseMode {
                name: "Кратко".into(),
                instructions: "Ответь одним предложением".into(),
            }],
        };
        modes.save(&path).unwrap();
        let loaded = ModesConfig::load(&path).unwrap();
        assert_eq!(loaded.modes.len(), 1);
        assert_eq!(loaded.modes[0].name, "Кратко");
        assert_eq!(loaded.modes[0].instructions, "Ответь одним предложением");
    }

    #[test]
    pub(crate) fn migrates_legacy_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
                "last_provider": "openai",
                "openai_api_key": "old-secret",
                "claude_api_key": null,
                "openai_model": "gpt-test",
                "claude_model": "claude-test"
            }"#,
        )
        .unwrap();

        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.key(Provider::Openai), Some("old-secret"));
        assert_eq!(loaded.model(Provider::Openai).unwrap(), "gpt-test");
        assert_eq!(
            loaded.temperature(Provider::Openai).unwrap(),
            default_temperature()
        );
        let migrated: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert!(migrated.get("providers").unwrap().is_array());
        assert!(migrated.get("openai_api_key").is_none());
    }

    #[test]
    pub(crate) fn loads_current_config_without_temperature() {
        let config: Config = serde_json::from_value(json!({
            "last_provider": "openai",
            "last_mode": null,
            "providers": [{
                "provider": "openai",
                "api_key": null,
                "model": "gpt-4.1-mini"
            }]
        }))
        .unwrap();

        assert_eq!(
            config.temperature(Provider::Openai).unwrap(),
            default_temperature()
        );
        assert_eq!(format_temperature(0.0), "0");
        assert_eq!(format_temperature(0.7), "0.7");
        assert_eq!(format_temperature(1.0), "1");
        assert_eq!(temperature_maximum(Provider::Openai, "gpt-4o"), 2.0);
        assert_eq!(temperature_maximum(Provider::Openai, "gpt-5-mini"), 1.0);
        assert!(supports_temperature_with_reasoning_none("gpt-5.6-luna"));
        assert!(!supports_temperature_with_reasoning_none("gpt-5-mini"));
        assert_eq!(
            temperature_maximum(Provider::Claude, "claude-sonnet-5"),
            1.0
        );
    }

    #[test]
    pub(crate) fn completes_slash_commands() {
        let history = DefaultHistory::new();
        let context = ReadlineContext::new(&history);
        let enabled = Arc::new(AtomicBool::new(false));
        let helper = CommandHelper::new(enabled.clone());
        let (start, candidates) = helper.complete("/pro", 4, &context).unwrap();
        assert_eq!(start, 0);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].replacement, "/provider");
        assert_eq!(candidates[1].replacement, "/profile");
        assert_eq!(helper.hint("/pro", 4, &context), None);
        assert_eq!(helper.hint("/prov", 5, &context).as_deref(), Some("ider"));
        assert!(helper.complete("/bra", 4, &context).unwrap().1.is_empty());
        assert_eq!(expand_command_hint("/pro", false), "/pro");
        assert_eq!(expand_command_hint("/prov", false), "/provider");
        assert_eq!(
            expand_command_hint("обычный запрос", false),
            "обычный запрос"
        );
        assert_eq!(expand_command_hint("/unknown", false), "/unknown");
        assert_eq!(expand_command_hint("/model", false), "/model");
        assert_eq!(expand_command_hint("/mode", false), "/mode");
        assert_eq!(expand_command_hint("/mem", false), "/memory");
        assert_eq!(expand_command_hint("/rem", false), "/remember");
        assert_eq!(expand_command_hint("/for", false), "/forget");
        assert_eq!(expand_command_hint("/tas", false), "/task");
        assert_eq!(helper.hint("/mode", 5, &context), None);
        enabled.store(true, Ordering::Relaxed);
        assert_eq!(
            helper.complete("/loa", 4, &context).unwrap().1[0].replacement,
            "/load"
        );
        assert_eq!(
            helper.complete("/bra", 4, &context).unwrap().1[0].replacement,
            "/branches"
        );
        assert_eq!(helper.hint("/swi", 4, &context).as_deref(), Some("tch"));
        assert_eq!(expand_command_hint("/check", true), "/checkpoint");
        assert_eq!(
            helper.highlight_hint("vider").as_ref(),
            "\x1b[2mvider\x1b[0m"
        );
    }

    #[test]
    pub(crate) fn parses_sorts_and_deduplicates_model_ids() {
        let body = json!({
            "data": [
                {"id": "model-z"},
                {"id": "model-a", "display_name": "Model A"},
                {"id": "model-z"},
                {"display_name": "No id"},
                {"id": ""}
            ]
        });
        assert_eq!(
            parse_model_ids(&body, Provider::Claude).unwrap(),
            vec!["model-a".to_owned(), "model-z".to_owned()]
        );
        assert!(parse_model_ids(&json!({"models": []}), Provider::Openai).is_err());
    }

    #[test]
    pub(crate) fn filters_out_openai_models_for_other_apis() {
        let body = json!({
            "data": [
                {"id": "gpt-5.6-luna"},
                {"id": "o4-mini"},
                {"id": "ft:gpt-4.1:team:custom:id"},
                {"id": "gpt-3.5-turbo-instruct"},
                {"id": "gpt-3.5-turbo-instruct-0914"},
                {"id": "gpt-realtime"},
                {"id": "gpt-4o-mini-transcribe"},
                {"id": "gpt-image-1"},
                {"id": "o3-deep-research"},
                {"id": "text-embedding-3-small"},
                {"id": "omni-moderation-latest"},
                {"id": "davinci-002"}
            ]
        });
        assert_eq!(
            parse_model_ids(&body, Provider::Openai).unwrap(),
            vec![
                "ft:gpt-4.1:team:custom:id".to_owned(),
                "gpt-5.6-luna".to_owned(),
                "o4-mini".to_owned()
            ]
        );
    }

    #[test]
    pub(crate) fn normalizes_temperature_for_selected_model() {
        assert_eq!(normalized_temperature(Provider::Claude, "any", 1.7), 1.0);
        assert_eq!(
            normalized_temperature(Provider::Openai, "gpt-5-mini", 0.4),
            1.0
        );
        assert_eq!(
            normalized_temperature(Provider::Openai, "gpt-4.1", 1.7),
            1.7
        );
    }

    pub(crate) fn test_agent_settings() -> AgentSettings {
        AgentSettings {
            provider: Provider::Openai,
            api_key: "test-key".into(),
            model: "test-model".into(),
            temperature: 0.5,
            instructions: Some("Отвечай кратко".into()),
            compression_strategy: CompressionStrategy::Summary,
            context_messages: default_context_messages(),
        }
    }

    #[test]
    pub(crate) fn creates_independent_agents_in_pool() {
        let client = Client::new();
        let pool = AgentPool::new(3, client, test_agent_settings());

        assert_eq!(pool.agents.len(), 3);
        assert_eq!(
            pool.agents.iter().map(|agent| agent.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(pool.agents.iter().all(|agent| agent.history.is_empty()));
        assert!(pool
            .agents
            .iter()
            .all(|agent| agent.status == AgentStatus::Idle));
    }

    #[test]
    pub(crate) fn reconfiguring_pool_resets_every_agent() {
        let client = Client::new();
        let mut pool = AgentPool::new(2, client, test_agent_settings());
        for agent in &mut pool.agents {
            agent.history.push(Message {
                role: "user".to_owned(),
                content: "старый запрос".into(),
            });
            agent.status = AgentStatus::Completed;
        }

        let mut settings = test_agent_settings();
        settings.temperature = 0.9;
        settings.instructions = Some("Новый режим".into());
        pool.reconfigure(settings);

        assert!(pool.agents.iter().all(|agent| agent.history.is_empty()));
        assert!(pool
            .agents
            .iter()
            .all(|agent| agent.status == AgentStatus::Idle));
        assert!(pool
            .agents
            .iter()
            .all(|agent| agent.settings.temperature == 0.9));
        assert!(pool
            .agents
            .iter()
            .all(|agent| agent.settings.instructions.as_deref() == Some("Новый режим")));
    }

    #[test]
    pub(crate) fn summary_keeps_recent_messages_and_original_archive() {
        let mut agent = Agent::new(1, Client::new(), test_agent_settings());
        let messages: Vec<_> = (0..24)
            .map(|i| Message {
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                content: format!("сообщение {i}"),
            })
            .collect();
        agent.restore(messages.clone());
        agent.apply_summary("Сохранённые решения", 14).unwrap();
        assert_eq!(agent.history, messages[14..]);
        assert_eq!(agent.persisted_history, messages);
        let instructions = agent.request_settings().instructions.unwrap();
        assert!(instructions.contains("Отвечай кратко"));
        assert!(instructions.contains("Сохранённые решения"));
        assert!(!instructions.contains("сообщение 0"));
        agent.reset();
        assert!(agent.summary.is_empty());
    }

    #[test]
    pub(crate) fn empty_summary_does_not_discard_context_and_unicode_limit_is_exact() {
        let mut agent = Agent::new(1, Client::new(), test_agent_settings());
        agent.restore(vec![Message {
            role: "user".into(),
            content: "важные данные".into(),
        }]);
        agent.summary = "Прежнее summary".into();
        assert!(agent.apply_summary("  ", 1).is_err());
        assert_eq!(agent.history.len(), 1);
        assert_eq!(agent.summary, "Прежнее summary");
        agent
            .apply_summary(&"я🦊".repeat(SUMMARY_MAX_CHARS), 1)
            .unwrap();
        assert_eq!(agent.summary.chars().count(), SUMMARY_MAX_CHARS);
        assert!(agent.history.is_empty());
    }

    #[test]
    pub(crate) fn changing_summary_window_restores_archive() {
        let mut agent = Agent::new(1, Client::new(), test_agent_settings());
        let messages = vec![
            Message {
                role: "user".into(),
                content: "Fact".into()
            };
            20
        ];
        agent.restore(messages.clone());
        agent.apply_summary("Facts", 10).unwrap();
        agent.set_compression(CompressionStrategy::Summary, 15);
        assert_eq!(agent.history, messages);
        assert!(agent.summary.is_empty());
        assert_eq!(
            agent
                .history
                .len()
                .saturating_sub(agent.settings.context_messages),
            5
        );
        agent.apply_summary("Facts", 5).unwrap();
        agent.set_compression(CompressionStrategy::Summary, 3);
        assert_eq!(
            agent
                .history
                .len()
                .saturating_sub(agent.settings.context_messages),
            17
        );
    }

    #[test]
    pub(crate) fn validates_compression_command_and_strategies() {
        for value in ["1001", "-1", "1.5", "abc", ""] {
            assert!(parse_context_messages(value).is_err(), "{value}");
        }
        assert_eq!(parse_context_messages("0").unwrap(), 0);
        assert_eq!(parse_context_messages("1").unwrap(), 1);
        assert_eq!(parse_context_messages("1000").unwrap(), 1000);
        assert_eq!(
            parse_compression_strategy("sliding").unwrap(),
            CompressionStrategy::SlidingWindow
        );
        assert_eq!(
            parse_compression_strategy("facts").unwrap(),
            CompressionStrategy::StickyFacts
        );
        assert_eq!(expand_command_hint("/com", false), "/compression");
    }

    #[test]
    pub(crate) fn sliding_window_keeps_only_recent_context_and_preserves_archive() {
        let mut agent = Agent::new(1, Client::new(), test_agent_settings());
        let messages = vec![
            Message {
                role: "user".into(),
                content: "Fact".into()
            };
            30
        ];
        agent.restore(messages.clone());
        agent.set_compression(CompressionStrategy::SlidingWindow, 6);
        agent.keep_recent_messages(0);
        assert_eq!(agent.history, messages[24..]);
        assert_eq!(agent.persisted_history, messages);
    }

    #[test]
    pub(crate) fn compression_strategy_survives_config_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let config = Config {
            compression_strategy: CompressionStrategy::StickyFacts,
            context_messages: 7,
            ..Config::default()
        };
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(
            loaded.compression_strategy,
            CompressionStrategy::StickyFacts
        );
        assert_eq!(loaded.context_messages, 7);
    }

    #[tokio::test]
    async fn short_history_needs_no_summary_request() {
        let mut agent = Agent::new(1, Client::new(), test_agent_settings());
        agent.restore(vec![
            Message {
                role: "user".into(),
                content: "Привет".into()
            };
            default_context_messages()
        ]);
        agent.compress_summary().await.unwrap();
        assert_eq!(agent.history.len(), default_context_messages());
        assert!(agent.summary.is_empty());
    }

    #[test]
    pub(crate) fn sticky_facts_are_key_value_and_added_to_instructions() {
        let mut agent = Agent::new(1, Client::new(), test_agent_settings());
        agent.set_compression(CompressionStrategy::StickyFacts, 10);
        agent.facts = parse_facts("```json\n{\"goal\":\"MVP\",\"budget\":\"1M\"}\n```").unwrap();
        let instructions = agent.request_settings().instructions.unwrap();
        assert!(instructions.contains("Важные факты диалога"));
        assert!(instructions.contains("\"goal\": \"MVP\""));
    }

    #[test]
    pub(crate) fn branching_restores_independent_histories_from_checkpoint() {
        let mut agent = Agent::new(1, Client::new(), test_agent_settings());
        agent.restore(vec![Message {
            role: "user".into(),
            content: "общая часть".into(),
        }]);
        agent.set_compression(CompressionStrategy::Branching, 10);
        agent.create_checkpoint();
        assert!(agent.branch_pending);
        agent.start_branch("вариант A").unwrap();
        agent.persisted_history.push(Message {
            role: "assistant".into(),
            content: "только A".into(),
        });
        agent.history = agent.persisted_history.clone();
        agent.load_checkpoint().unwrap();
        assert!(agent.branch_pending);
        agent.start_branch("вариант B").unwrap();
        assert_eq!(agent.persisted_history.len(), 1);
        assert_eq!(agent.persisted_history[0].content, "общая часть");
        agent.switch_branch("вариант A").unwrap();
        assert_eq!(agent.persisted_history.last().unwrap().content, "только A");
    }

    #[test]
    pub(crate) fn loading_checkpoint_exposes_last_assistant_message_before_it() {
        let mut agent = Agent::new(1, Client::new(), test_agent_settings());
        agent.restore(vec![
            Message {
                role: "user".into(),
                content: "общая часть".into(),
            },
            Message {
                role: "assistant".into(),
                content: "ответ до checkpoint".into(),
            },
        ]);
        agent.set_compression(CompressionStrategy::Branching, 10);
        agent.create_checkpoint();
        agent.start_branch("новая ветка").unwrap();
        agent.persisted_history.push(Message {
            role: "assistant".into(),
            content: "ответ после checkpoint".into(),
        });

        agent.load_checkpoint().unwrap();

        assert_eq!(agent.last_assistant_message(), Some("ответ до checkpoint"));
    }

    #[test]
    pub(crate) fn sqlite_persists_all_branches_checkpoint_and_active_branch() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::open(&directory.path().join("sessions.db")).unwrap();
        let mut agent = Agent::new(1, Client::new(), test_agent_settings());
        agent.restore(vec![Message {
            role: "user".into(),
            content: "общая часть".into(),
        }]);
        agent.set_compression(CompressionStrategy::Branching, 10);
        agent.create_checkpoint();
        agent.start_branch("вариант A").unwrap();
        agent.persisted_history.push(Message {
            role: "assistant".into(),
            content: "только A".into(),
        });
        agent.history = agent.persisted_history.clone();
        agent.load_checkpoint().unwrap();
        agent.start_branch("вариант B").unwrap();
        agent.switch_branch("вариант A").unwrap();

        let id = store
            .save(
                None,
                SessionSnapshot {
                    provider: Provider::Openai,
                    model: "test-model",
                    mode: None,
                    temperature: 1.0,
                    messages: &agent.persisted_history,
                    summary: &agent.summary,
                    facts: &agent.facts,
                    summarized_count: 0,
                    compression_strategy: CompressionStrategy::Branching,
                    context_messages: 10,
                    branches: &agent.branches,
                    checkpoint: agent.checkpoint.as_ref(),
                    active_branch: &agent.active_branch,
                    branch_pending: agent.branch_pending,
                    profile_id: None,
                    task_id: None,
                },
            )
            .unwrap();
        let loaded = store.load(id).unwrap();
        assert_eq!(loaded.active_branch, "вариант A");
        assert!(!loaded.branch_pending);
        assert_eq!(loaded.branches.len(), 3);
        assert_eq!(
            loaded.branches["вариант A"]
                .persisted_history
                .last()
                .unwrap()
                .content,
            "только A"
        );
        assert_eq!(loaded.branches["вариант B"].persisted_history.len(), 1);
        assert_eq!(
            loaded.checkpoint.unwrap().persisted_history[0].content,
            "общая часть"
        );

        agent.load_checkpoint().unwrap();
        store.save_branching_state(id, &agent).unwrap();
        let loaded = store.load(id).unwrap();
        assert!(loaded.branch_pending);
        assert_eq!(
            loaded.messages,
            loaded.checkpoint.unwrap().persisted_history
        );
    }

    #[test]
    pub(crate) fn summary_is_saved_separately_and_old_sessions_still_load() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.db");
        let store = SessionStore::open(&path).unwrap();
        let messages = vec![
            Message {
                role: "user".into(),
                content: "Архив".into()
            };
            12
        ];
        let id = store
            .save(
                None,
                SessionSnapshot {
                    provider: Provider::Claude,
                    model: "model",
                    mode: None,
                    temperature: 1.0,
                    messages: &messages,
                    summary: "Факты",
                    facts: &BTreeMap::from([("goal".to_owned(), "MVP".to_owned())]),
                    summarized_count: 2,
                    compression_strategy: CompressionStrategy::StickyFacts,
                    context_messages: 7,
                    branches: &HashMap::new(),
                    checkpoint: None,
                    active_branch: "main",
                    branch_pending: false,
                    profile_id: None,
                    task_id: None,
                },
            )
            .unwrap();
        drop(store);
        let store = SessionStore::open(&path).unwrap();
        let session = store.load(id).unwrap();
        assert_eq!(session.summary, "Факты");
        assert_eq!(session.facts.get("goal").map(String::as_str), Some("MVP"));
        assert_eq!(session.summarized_count, 2);
        assert_eq!(
            session.compression_strategy,
            CompressionStrategy::StickyFacts
        );
        assert_eq!(session.context_messages, 7);
        assert_eq!(session.messages, messages);
        store
            .connection
            .execute("DELETE FROM session_context WHERE session_id = ?1", [id])
            .unwrap();
        let legacy = store.load(id).unwrap();
        assert!(legacy.summary.is_empty());
        assert_eq!(legacy.summarized_count, 0);
        assert_eq!(legacy.messages, messages);
        assert_eq!(legacy.compression_strategy, CompressionStrategy::Summary);
        assert_eq!(legacy.context_messages, default_context_messages());
        store
            .save(
                Some(id),
                SessionSnapshot {
                    provider: Provider::Claude,
                    model: "model",
                    mode: None,
                    temperature: 1.0,
                    messages: &messages,
                    summary: "Новые факты",
                    facts: &BTreeMap::new(),
                    summarized_count: 4,
                    compression_strategy: CompressionStrategy::SlidingWindow,
                    context_messages: 5,
                    branches: &HashMap::new(),
                    checkpoint: None,
                    active_branch: "main",
                    branch_pending: false,
                    profile_id: None,
                    task_id: None,
                },
            )
            .unwrap();
        let updated = store.load(id).unwrap();
        assert_eq!(updated.summary, "Новые факты");
        assert_eq!(
            updated.compression_strategy,
            CompressionStrategy::SlidingWindow
        );
        assert_eq!(updated.context_messages, 5);
        store
            .update_compression(id, CompressionStrategy::Summary, 20)
            .unwrap();
        let recomposed = store.load(id).unwrap();
        assert!(recomposed.summary.is_empty());
        assert_eq!(recomposed.summarized_count, 0);
        assert_eq!(
            recomposed.compression_strategy,
            CompressionStrategy::Summary
        );
        assert_eq!(recomposed.context_messages, 20);
        store.delete(id).unwrap();
        let count: usize = store
            .connection
            .query_row("SELECT COUNT(*) FROM session_context", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    pub(crate) fn toon_history_round_trips_multiline_messages() {
        let messages = vec![
            Message {
                role: "user".into(),
                content: "Привет, лиса!\nКак дела?".into(),
            },
            Message {
                role: "assistant".into(),
                content: "Хорошо: \"отлично\"".into(),
            },
        ];
        let encoded = encode_messages_toon(&messages);
        assert!(encoded.starts_with("messages[2]{role,content}:"));
        assert_eq!(decode_messages_toon(&encoded).unwrap(), messages);
    }

    #[test]
    pub(crate) fn sqlite_store_saves_and_loads_session() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::open(&directory.path().join("sessions.db")).unwrap();
        let messages = vec![Message {
            role: "user".into(),
            content: "Обсудим сохранение контекста".into(),
        }];
        let id = store
            .save(
                None,
                SessionSnapshot {
                    provider: Provider::Openai,
                    model: "test-model",
                    mode: Some("Кратко"),
                    temperature: 0.5,
                    messages: &messages,
                    summary: "",
                    facts: &BTreeMap::new(),
                    summarized_count: 0,
                    compression_strategy: CompressionStrategy::Summary,
                    context_messages: 10,
                    branches: &HashMap::new(),
                    checkpoint: None,
                    active_branch: "main",
                    branch_pending: false,
                    profile_id: None,
                    task_id: None,
                },
            )
            .unwrap();
        let loaded = store.load(id).unwrap();
        assert_eq!(loaded.messages, messages);
        assert_eq!(loaded.model, "test-model");
        assert_eq!(loaded.mode.as_deref(), Some("Кратко"));
        store
            .update_compression(id, CompressionStrategy::Branching, 42)
            .unwrap();
        let loaded = store.load(id).unwrap();
        assert_eq!(loaded.compression_strategy, CompressionStrategy::Branching);
        assert_eq!(loaded.context_messages, 42);
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(store.delete(id).unwrap());
        assert!(store.list().unwrap().is_empty());
        assert!(!store.delete(id).unwrap());
    }

    #[test]
    fn task_phases_are_strict_and_sequential() {
        let phases = [
            TaskPhase::Planning,
            TaskPhase::Execution,
            TaskPhase::Validation,
            TaskPhase::Done,
        ];
        assert_eq!(TaskPhase::Planning.next(), Some(TaskPhase::Execution));
        assert_eq!(TaskPhase::Execution.next(), Some(TaskPhase::Validation));
        assert_eq!(TaskPhase::Validation.next(), Some(TaskPhase::Done));
        assert_eq!(TaskPhase::Done.next(), None);
        for phase in phases {
            assert_eq!(phase.to_string().parse::<TaskPhase>().unwrap(), phase);
            assert!(!phase.instructions().is_empty());
        }
        assert!(TaskPhase::Planning
            .instructions()
            .contains("не переходи к выполнению"));
        assert!(TaskPhase::Execution
            .instructions()
            .contains("согласованный TODO"));
        assert!(TaskPhase::Validation
            .instructions()
            .contains("проверяй результат"));
        assert!(TaskPhase::Done
            .instructions()
            .contains("Следующего этапа нет"));
        assert!("review".parse::<TaskPhase>().is_err());
    }

    #[test]
    fn memory_text_validation_is_trimmed_unicode_aware_and_bounded() {
        assert_eq!(
            validate_memory_text("  профиль  ", "поле", 8).unwrap(),
            "профиль"
        );
        assert!(validate_memory_text("   ", "поле", 10).is_err());
        assert!(validate_memory_text(&"я".repeat(11), "поле", 10).is_err());
        assert!(validate_profile("имя", " ").is_err());
        assert!(validate_task(" ", "TODO").is_err());
        assert_eq!(
            validate_memory_entry("  Пользователь пишет на Rust  ").unwrap(),
            "Пользователь пишет на Rust"
        );
        assert!(validate_memory_entry(" ").is_err());
        assert!(validate_memory_entry(&"я".repeat(MEMORY_ENTRY_MAX_CHARS + 1)).is_err());
    }

    #[test]
    fn structured_profile_instructions_are_canonical_and_bounded() {
        let prompt = format_profile_field_prompt("Стиль ответа", true);
        assert!(prompt.contains("Стиль ответа"));
        assert!(prompt.contains("\x1b[33m"));
        assert!(prompt.contains("\x1b[1m"));

        let instructions =
            compose_profile_instructions("  кратко  ", "списком", "без эмодзи").unwrap();
        assert_eq!(
            instructions,
            "Стиль:\nкратко\n\nФормат:\nсписком\n\nОграничения:\nбез эмодзи"
        );
        assert!(compose_profile_instructions("", "списком", "без эмодзи").is_err());
        assert!(compose_profile_instructions(
            &"я".repeat(PROFILE_INSTRUCTIONS_MAX_CHARS),
            "списком",
            "без эмодзи"
        )
        .is_err());
    }

    #[test]
    fn long_term_facts_round_trip_and_survive_database_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("facts.db");
        let store = SessionStore::open(&path).unwrap();
        let first = store
            .create_memory_entry("  Пользователь пишет на Rust  ")
            .unwrap();
        let second = store
            .create_memory_entry("Проект использует SQLite")
            .unwrap();
        assert_eq!(first.content, "Пользователь пишет на Rust");
        assert_eq!(
            store.list_memory_entries().unwrap(),
            vec![first.clone(), second.clone()]
        );
        drop(store);

        let store = SessionStore::open(&path).unwrap();
        assert_eq!(
            store.list_memory_entries().unwrap(),
            vec![first.clone(), second.clone()]
        );
        store.delete_memory_entry(first.id).unwrap();
        assert_eq!(store.list_memory_entries().unwrap(), vec![second]);
        assert!(store.load_memory_entry(first.id).is_err());
        assert!(store.delete_memory_entry(999).is_err());
    }

    #[test]
    fn profiles_tasks_and_session_links_survive_database_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.db");
        let store = SessionStore::open(&path).unwrap();
        let profile = store
            .create_profile("Rust", "Отвечай как Rust-разработчик")
            .unwrap();
        assert!(store.create_profile("Rust", "Дублирующее имя").is_err());
        let mut task = store
            .create_task("Memory layers", "Спроектировать и реализовать")
            .unwrap();
        assert_eq!(task.phase, TaskPhase::Planning);
        task = store
            .update_task_todo(task.id, "Реализовать и проверить")
            .unwrap();
        assert_eq!(task.todo, "Реализовать и проверить");
        for expected in [TaskPhase::Execution, TaskPhase::Validation, TaskPhase::Done] {
            task = store.advance_task(task.id).unwrap();
            assert_eq!(task.phase, expected);
        }
        assert!(store.advance_task(task.id).is_err());

        let messages = vec![Message {
            role: "user".into(),
            content: "Продолжим задачу".into(),
        }];
        let session_id = store
            .save(
                None,
                SessionSnapshot {
                    provider: Provider::Openai,
                    model: "test-model",
                    mode: None,
                    temperature: 1.0,
                    messages: &messages,
                    summary: "summary",
                    facts: &BTreeMap::from([("fact".to_owned(), "value".to_owned())]),
                    summarized_count: 0,
                    compression_strategy: CompressionStrategy::Branching,
                    context_messages: 10,
                    branches: &HashMap::new(),
                    checkpoint: None,
                    active_branch: "main",
                    branch_pending: false,
                    profile_id: Some(profile.id),
                    task_id: Some(task.id),
                },
            )
            .unwrap();
        let second_messages = vec![Message {
            role: "user".into(),
            content: "Новая краткосрочная память той же задачи".into(),
        }];
        let second_session_id = store
            .save(
                None,
                SessionSnapshot {
                    provider: Provider::Claude,
                    model: "another-model",
                    mode: None,
                    temperature: 0.5,
                    messages: &second_messages,
                    summary: "",
                    facts: &BTreeMap::new(),
                    summarized_count: 0,
                    compression_strategy: CompressionStrategy::Summary,
                    context_messages: 10,
                    branches: &HashMap::new(),
                    checkpoint: None,
                    active_branch: "main",
                    branch_pending: false,
                    profile_id: Some(profile.id),
                    task_id: Some(task.id),
                },
            )
            .unwrap();
        drop(store);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute("DROP TABLE memory_entries", []).unwrap();
        drop(connection);

        let store = SessionStore::open(&path).unwrap();
        assert!(store.list_memory_entries().unwrap().is_empty());
        assert_eq!(store.list_profiles().unwrap(), vec![profile.clone()]);
        assert_eq!(store.list_tasks().unwrap(), vec![task.clone()]);
        let loaded = store.load(session_id).unwrap();
        assert_eq!(loaded.profile, Some(profile.clone()));
        assert_eq!(loaded.task, Some(task.clone()));
        assert_eq!(loaded.summary, "summary");
        assert_eq!(loaded.facts.get("fact").map(String::as_str), Some("value"));
        assert_eq!(loaded.messages, messages);
        let second = store.load(second_session_id).unwrap();
        assert_eq!(
            second.profile.as_ref().map(|value| value.id),
            Some(profile.id)
        );
        assert_eq!(second.task.as_ref().map(|value| value.id), Some(task.id));
        assert_eq!(second.messages, second_messages);
    }

    #[test]
    fn legacy_session_schema_migrates_without_memory_links() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    title TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    model TEXT NOT NULL,
                    mode TEXT,
                    temperature REAL NOT NULL,
                    history_toon TEXT NOT NULL,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions
                 (title, provider, model, temperature, history_toon)
                 VALUES ('legacy', 'openai', 'old-model', 1.0, 'messages[0]{role,content}:')",
                [],
            )
            .unwrap();
        drop(connection);

        let store = SessionStore::open(&path).unwrap();
        let loaded = store.load(1).unwrap();
        assert!(loaded.profile.is_none());
        assert!(loaded.task.is_none());
        assert!(loaded.messages.is_empty());
        assert!(store.list_memory_entries().unwrap().is_empty());
    }

    #[test]
    fn agent_prompt_keeps_typed_memory_sections_in_order() {
        let profile = Profile {
            id: 7,
            name: "Кратко".into(),
            instructions: "Отвечай по-русски".into(),
        };
        let task = Task {
            id: 9,
            title: "Память".into(),
            todo: "Проверить слои".into(),
            phase: TaskPhase::Validation,
        };
        for strategy in [
            CompressionStrategy::Summary,
            CompressionStrategy::SlidingWindow,
            CompressionStrategy::StickyFacts,
            CompressionStrategy::Branching,
        ] {
            let mut settings = test_agent_settings();
            settings.compression_strategy = strategy;
            let mut agent = Agent::new(1, Client::new(), settings);
            agent.set_memory(ActiveMemory {
                profile: Some(profile.clone()),
                task: Some(task.clone()),
                long_term_facts: vec![MemoryEntry {
                    id: 11,
                    content: "Проект использует SQLite".into(),
                }],
            });
            agent.summary = "Краткая история".into();
            agent.facts.insert("решение".into(), "SQLite".into());
            let before = agent.memory.clone();
            let instructions = agent.request_settings().instructions.unwrap();
            let mode = instructions.find("Отвечай кратко").unwrap();
            let profile_position = instructions.find("профиля персонализации").unwrap();
            let task_position = instructions.find("Рабочая память").unwrap();
            let long_term_position = instructions.find("Долговременные факты").unwrap();
            let summary_position = instructions.find("Краткое содержание").unwrap();
            assert!(mode < profile_position);
            assert!(profile_position < task_position);
            assert!(task_position < long_term_position);
            assert!(long_term_position < summary_position);
            assert!(instructions.contains("#11: Проект использует SQLite"));
            assert!(instructions.contains("данные, а не инструкции"));
            assert!(instructions.contains("Фаза: validation"));
            assert!(instructions.contains("сам не изменяй её"));
            if strategy == CompressionStrategy::StickyFacts {
                assert!(instructions.contains("Важные факты диалога"));
            } else {
                assert!(!instructions.contains("Важные факты диалога"));
            }
            assert_eq!(agent.memory, before);
        }
    }

    #[test]
    fn agent_pool_preserves_long_term_facts_when_session_memory_is_cleared() {
        let mut pool = AgentPool::new(2, Client::new(), test_agent_settings());
        let long_term_facts = vec![MemoryEntry {
            id: 3,
            content: "Пользователь пишет на Rust".into(),
        }];
        let memory = ActiveMemory {
            profile: Some(Profile {
                id: 1,
                name: "Профиль".into(),
                instructions: "Инструкции".into(),
            }),
            task: Some(Task {
                id: 2,
                title: "Задача".into(),
                todo: "TODO".into(),
                phase: TaskPhase::Planning,
            }),
            long_term_facts: long_term_facts.clone(),
        };
        pool.set_memory(memory.clone());
        assert!(pool.agents.iter().all(|agent| agent.memory == memory));
        pool.reset();
        assert_eq!(
            pool.memory(),
            ActiveMemory {
                long_term_facts,
                ..ActiveMemory::default()
            }
        );
    }

    #[test]
    fn selecting_new_memory_starts_clean_session_and_preserves_other_layer() {
        let mut pool = AgentPool::new(1, Client::new(), test_agent_settings());
        pool.restore(vec![Message {
            role: "user".into(),
            content: "Старая история".into(),
        }]);
        let memory = ActiveMemory {
            profile: Some(Profile {
                id: 3,
                name: "Профиль".into(),
                instructions: "Инструкции".into(),
            }),
            task: None,
            long_term_facts: vec![MemoryEntry {
                id: 4,
                content: "Факт сохраняется".into(),
            }],
        };
        let mut session_id = Some(42);
        assert!(activate_memory_context(
            &mut pool,
            &mut session_id,
            memory.clone()
        ));
        assert!(pool.persisted_history().is_empty());
        assert_eq!(session_id, None);
        assert_eq!(pool.memory(), memory);
    }

    #[test]
    fn memory_view_and_phase_colors_are_explicit() {
        let memory = ActiveMemory {
            profile: Some(Profile {
                id: 1,
                name: "Профиль".into(),
                instructions: "Только русский".into(),
            }),
            task: Some(Task {
                id: 2,
                title: "Задача".into(),
                todo: "Проверить вывод".into(),
                phase: TaskPhase::Execution,
            }),
            long_term_facts: vec![MemoryEntry {
                id: 8,
                content: "Проект использует SQLite".into(),
            }],
        };
        let view = format_memory(&memory, 4);
        assert!(view.contains("Краткосрочная память (сессия): 4 сообщений"));
        assert!(view.contains("Рабочая память (задача)"));
        assert!(view.contains("Долговременная память (факты)"));
        assert!(view.contains("#8: Проект использует SQLite"));
        assert!(view.contains("Профиль персонализации"));
        assert!(!view.contains("api_key"));

        for (phase, ansi) in [
            (TaskPhase::Planning, "\x1b[34m"),
            (TaskPhase::Execution, "\x1b[33m"),
            (TaskPhase::Validation, "\x1b[35m"),
            (TaskPhase::Done, "\x1b[32m"),
        ] {
            let line = format_task_phase_line(phase, true);
            assert!(line.contains("Этап задачи:"));
            assert!(line.contains(&phase.to_string()));
            assert!(line.contains(ansi), "{phase}: {line:?}");
        }
        assert!(active_task_phase_line(&memory, true).is_some());
        assert!(active_task_phase_line(&ActiveMemory::default(), true).is_none());
        assert_eq!(status_clear_sequence(false), "\r\x1b[2K");
        assert_eq!(status_clear_sequence(true), "\r\x1b[2K\x1b[1A\r\x1b[2K");
        let two_lines = status_render_sequence(Some("Этап задачи: planning"), "Статус");
        assert!(two_lines.find("Этап задачи").unwrap() < two_lines.find("Статус").unwrap());
        assert!(two_lines.ends_with("\x1b[2A\r"));
        let one_line = status_render_sequence(None, "Статус");
        assert!(!one_line.contains("Этап задачи"));
        assert!(one_line.ends_with("\x1b[1A\r"));
    }

    #[test]
    fn deterministic_memory_commands_select_saved_records() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::open(&directory.path().join("commands.db")).unwrap();
        let profile = store.create_profile("Reviewer", "Проверяй факты").unwrap();
        let task = store.create_task("Команды", "Проверить выбор").unwrap();

        assert_eq!(
            handle_profile_command("/profile use Reviewer", &store, None).unwrap(),
            ProfileCommandOutcome::Select(Some(profile.clone()))
        );
        assert_eq!(
            handle_profile_command("/profile off", &store, Some(&profile)).unwrap(),
            ProfileCommandOutcome::Select(None)
        );
        assert!(handle_profile_command("/profile unknown", &store, None).is_err());
        assert_eq!(
            handle_task_command("/task use 1", &store, None).unwrap(),
            TaskCommandOutcome::Select(Some(task.clone()))
        );
        assert_eq!(
            handle_task_command("/task off", &store, Some(&task)).unwrap(),
            TaskCommandOutcome::Select(None)
        );
        assert!(handle_task_command("/task skip", &store, Some(&task)).is_err());
        let fact = handle_remember_command("/remember Пользователь пишет на Rust", &store)
            .unwrap()
            .unwrap();
        assert_eq!(fact.content, "Пользователь пишет на Rust");
        assert_eq!(store.list_memory_entries().unwrap(), vec![fact]);

        assert!(advance_task_if_confirmed(&store, &task, false)
            .unwrap()
            .is_none());
        assert_eq!(store.load_task(task.id).unwrap().phase, TaskPhase::Planning);
        let advanced = advance_task_if_confirmed(&store, &task, true)
            .unwrap()
            .unwrap();
        assert_eq!(advanced.phase, TaskPhase::Execution);
    }

    #[test]
    fn cancelled_memory_changes_do_not_write_to_database() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::open(&directory.path().join("cancel.db")).unwrap();
        assert!(
            create_profile_if_confirmed(&store, "Не сохранять", "Инструкции", false)
                .unwrap()
                .is_none()
        );
        assert!(
            create_task_if_confirmed(&store, "Не сохранять", "TODO", false)
                .unwrap()
                .is_none()
        );
        assert!(
            create_memory_entry_if_confirmed(&store, "Не сохранять", false)
                .unwrap()
                .is_none()
        );
        assert!(store.list_profiles().unwrap().is_empty());
        assert!(store.list_tasks().unwrap().is_empty());
        assert!(store.list_memory_entries().unwrap().is_empty());

        let task = store.create_task("Сохранённая", "Старый TODO").unwrap();
        assert!(
            update_task_todo_if_confirmed(&store, &task, "Новый TODO", false)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.load_task(task.id).unwrap().todo, "Старый TODO");
    }

    #[test]
    fn new_session_clears_active_memory_but_not_saved_records() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::open(&directory.path().join("new-session.db")).unwrap();
        let profile = store.create_profile("Профиль", "Инструкции").unwrap();
        let task = store.create_task("Задача", "TODO").unwrap();
        let fact = store.create_memory_entry("Факт переживает /new").unwrap();
        let mut pool = AgentPool::new(1, Client::new(), test_agent_settings());
        pool.set_memory(ActiveMemory {
            profile: Some(profile.clone()),
            task: Some(task.clone()),
            long_term_facts: vec![fact.clone()],
        });
        pool.reset();

        assert_eq!(
            pool.memory(),
            ActiveMemory {
                long_term_facts: vec![fact.clone()],
                ..ActiveMemory::default()
            }
        );
        assert_eq!(store.list_profiles().unwrap(), vec![profile]);
        assert_eq!(store.list_tasks().unwrap(), vec![task]);
        assert_eq!(store.list_memory_entries().unwrap(), vec![fact]);
    }

    #[test]
    fn remember_and_forget_only_change_long_term_facts() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::open(&directory.path().join("isolation.db")).unwrap();
        let mut pool = AgentPool::new(1, Client::new(), test_agent_settings());
        let profile = store
            .create_profile("Профиль", "Старые инструкции")
            .unwrap();
        let task = store.create_task("Задача", "Не менять TODO").unwrap();
        pool.restore(vec![Message {
            role: "user".into(),
            content: "История".into(),
        }]);
        pool.set_memory(ActiveMemory {
            profile: Some(profile.clone()),
            task: Some(task.clone()),
            ..ActiveMemory::default()
        });
        pool.agents[0].summary = "Сводка".into();
        pool.agents[0].facts.insert("session".into(), "fact".into());
        let history = pool.agents[0].persisted_history.clone();
        let summary = pool.agents[0].summary.clone();
        let session_facts = pool.agents[0].facts.clone();

        let entry = handle_remember_command("/remember Новый факт", &store)
            .unwrap()
            .unwrap();
        let mut memory = pool.memory();
        memory.long_term_facts = store.list_memory_entries().unwrap();
        pool.set_memory(memory);

        assert_eq!(pool.agents[0].persisted_history, history);
        assert_eq!(pool.agents[0].summary, summary);
        assert_eq!(pool.agents[0].facts, session_facts);
        assert_eq!(pool.memory().profile, Some(profile));
        assert_eq!(pool.memory().task, Some(task));
        assert_eq!(pool.memory().long_term_facts, vec![entry.clone()]);

        assert!(!forget_memory_entry_if_confirmed(&store, &entry, false).unwrap());
        assert_eq!(store.list_memory_entries().unwrap(), vec![entry.clone()]);
        assert!(forget_memory_entry_if_confirmed(&store, &entry, true).unwrap());
        assert!(store.list_memory_entries().unwrap().is_empty());
    }

    #[test]
    fn provider_payloads_preserve_system_instructions() {
        let history = vec![Message {
            role: "user".into(),
            content: "Контрольный запрос".into(),
        }];
        let mut settings = test_agent_settings();
        settings.model = "gpt-5.6-luna".into();
        let openai = build_openai_payload(&settings, &history);
        assert_eq!(openai["model"], "gpt-5.6-luna");
        assert_eq!(openai["temperature"], 0.5);
        assert_eq!(openai["input"][0]["content"], "Контрольный запрос");
        assert_eq!(openai["instructions"], "Отвечай кратко");
        assert_eq!(openai["reasoning"]["effort"], "none");

        settings.provider = Provider::Claude;
        settings.model = "claude-test".into();
        let claude = build_claude_payload(&settings, &history);
        assert_eq!(claude["model"], "claude-test");
        assert_eq!(claude["temperature"], 0.5);
        assert_eq!(claude["messages"][0]["content"], "Контрольный запрос");
        assert_eq!(claude["system"], "Отвечай кратко");
        assert_eq!(claude["max_tokens"], 4096);

        settings.instructions = None;
        assert!(build_openai_payload(&settings, &history)
            .get("instructions")
            .is_none());
        assert!(build_claude_payload(&settings, &history)
            .get("system")
            .is_none());
    }

    #[test]
    fn contrasting_profiles_change_every_provider_payload() {
        let concise =
            compose_profile_instructions("кратко", "не более трёх пунктов", "без эмодзи").unwrap();
        let mentor = compose_profile_instructions(
            "обучающе",
            "пошагово с примером",
            "объяснять новые термины",
        )
        .unwrap();
        let history = vec![Message {
            role: "user".into(),
            content: "Объясни Arc<Mutex<T>>".into(),
        }];
        let mut payloads = Vec::new();
        for (id, name, instructions) in [(1, "Кратко", concise), (2, "Наставник", mentor)]
        {
            let mut agent = Agent::new(1, Client::new(), test_agent_settings());
            agent.set_memory(ActiveMemory {
                profile: Some(Profile {
                    id,
                    name: name.into(),
                    instructions,
                }),
                long_term_facts: vec![MemoryEntry {
                    id: 5,
                    content: "Пользователь пишет на Rust".into(),
                }],
                ..ActiveMemory::default()
            });
            for _ in 0..2 {
                let request_settings = agent.request_settings();
                let openai = build_openai_payload(&request_settings, &history);
                assert!(openai["instructions"]
                    .as_str()
                    .unwrap()
                    .contains("Инструкции профиля персонализации"));
                assert!(openai["instructions"]
                    .as_str()
                    .unwrap()
                    .contains("Долговременные факты"));
                let mut claude_settings = request_settings.clone();
                claude_settings.provider = Provider::Claude;
                let claude = build_claude_payload(&claude_settings, &history);
                assert_eq!(openai["instructions"], claude["system"]);
                payloads.push(openai["instructions"].clone());
            }
        }
        assert_eq!(payloads[0], payloads[1]);
        assert_eq!(payloads[2], payloads[3]);
        assert_ne!(payloads[0], payloads[2]);
    }
}
