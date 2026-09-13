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
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "/provider");
        assert_eq!(helper.hint("/pro", 4, &context).as_deref(), Some("vider"));
        assert!(helper.complete("/bra", 4, &context).unwrap().1.is_empty());
        assert_eq!(expand_command_hint("/pro", false), "/provider");
        assert_eq!(
            expand_command_hint("обычный запрос", false),
            "обычный запрос"
        );
        assert_eq!(expand_command_hint("/unknown", false), "/unknown");
        assert_eq!(expand_command_hint("/model", false), "/model");
        assert_eq!(expand_command_hint("/mode", false), "/mode");
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
}
