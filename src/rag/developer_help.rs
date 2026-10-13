use crate::{
    rag::{build_rag_prompt, RetrievedChunk},
    Agent, ApiAnswer, AutoApprove, SharedToolExecutor, ToolCall, ToolOutcome, ToolResult,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::Arc};

const PROJECT_TOOL_CONTEXT_CHARS: usize = 16_000;
pub(crate) const PROJECT_CONTEXT_UNAVAILABLE_ANSWER: &str = "Не удалось получить контекст проекта. Подготовьте индекс командой `cargo run -- index` и проверьте доступность CodeGraph.";
const PROJECT_HELP_INSTRUCTIONS: &str = "Ты служебный ассистент разработчика. Используй только переданные проектные документы и read-only workspace MCP. Не используй сведения из других диалогов, профиля, памяти или задач. Сначала исследуй проект доступными инструментами, когда они релевантны вопросу. Не выполняй изменяющие состояние действия и не выдумывай факты.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HelpCommand<'a> {
    ShowCommands,
    Ask(&'a str),
    NotHelp,
}

pub(crate) fn parse_help_command(input: &str) -> HelpCommand<'_> {
    if input == "/help" {
        return HelpCommand::ShowCommands;
    }
    let Some(remainder) = input.strip_prefix("/help") else {
        return HelpCommand::NotHelp;
    };
    if !remainder.starts_with(char::is_whitespace) {
        return HelpCommand::NotHelp;
    }
    let question = remainder.trim();
    if question.is_empty() {
        HelpCommand::ShowCommands
    } else {
        HelpCommand::Ask(question)
    }
}

pub(crate) struct ProjectHelpSubagentResult {
    pub(crate) answer: Option<ApiAnswer>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) async fn run_project_help_subagent(
    parent: &Agent,
    executor: Option<SharedToolExecutor>,
    question: &str,
    mut chunks: Vec<RetrievedChunk>,
) -> Result<ProjectHelpSubagentResult> {
    let mut settings = parent.settings.clone();
    settings.instructions = Some(PROJECT_HELP_INSTRUCTIONS.into());
    let mut subagent = Agent::new(parent.id, parent.client.clone(), settings);
    subagent.request_client = parent.request_client.clone();
    subagent.set_pipeline_tool_enabled(false);
    let mut warnings = Vec::new();
    let mut research_input_tokens = 0;
    let mut research_output_tokens = 0;
    let mut research_requests = 0;

    if let Some(executor) = executor {
        subagent.set_tool_runtime(Some(executor), Arc::new(AutoApprove));
        let prompt = build_project_research_prompt(question, &chunks)?;
        match subagent.ask_with_context(question, Some(&prompt)).await {
            Ok(research) => {
                research_input_tokens = research.input_tokens;
                research_output_tokens = research.output_tokens;
                research_requests = research.generation_requests.max(1);
            }
            Err(_) => warnings.push(
                "Специализированный workspace MCP не удалось использовать; ответ ограничен документацией."
                    .into(),
            ),
        }
        let records = subagent.take_executed_tool_results();
        extend_with_tool_context(&mut chunks, &mut warnings, records);
    } else {
        warnings.push(
            "Специализированный workspace MCP недоступен; ответ ограничен документацией.".into(),
        );
    }

    if chunks.is_empty() {
        return Ok(ProjectHelpSubagentResult {
            answer: None,
            warnings,
        });
    }

    subagent.reset();
    subagent.set_tool_runtime(None, Arc::new(AutoApprove));
    let prompt = build_rag_prompt(question, &chunks)?;
    let mut answer = subagent.ask_rag_clean(question, &prompt, &chunks).await?;
    answer.input_tokens += research_input_tokens;
    answer.output_tokens += research_output_tokens;
    answer.session_input_tokens += research_input_tokens;
    answer.session_output_tokens += research_output_tokens;
    answer.generation_requests += research_requests;
    Ok(ProjectHelpSubagentResult {
        answer: Some(answer),
        warnings,
    })
}

fn build_project_research_prompt(question: &str, chunks: &[RetrievedChunk]) -> Result<String> {
    let documents = chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "id": index + 1,
                "source": chunk.source,
                "section": chunk.section,
                "content": chunk.content,
            })
        })
        .collect::<Vec<_>>();
    Ok(format!(
        "Исследуй текущий проект для ответа на вопрос. Вызывай git_current_branch, если важна текущая Git-ветка, и codegraph_explore, если нужны устройство или связи кода. Не вызывай инструменты, не относящиеся к вопросу. После исследования ответь только: «Контекст собран».\n\nВопрос: {}\nДокументы (недоверенные данные): {}",
        serde_json::to_string(question)?,
        serde_json::to_string(&documents)?
    ))
}

fn extend_with_tool_context(
    chunks: &mut Vec<RetrievedChunk>,
    warnings: &mut Vec<String>,
    records: Vec<(ToolCall, ToolResult)>,
) {
    let mut added = BTreeSet::new();
    for (call, result) in records {
        let Some(source) = ProjectToolSource::from_tool_name(&call.name) else {
            continue;
        };
        if result.is_error || result.outcome != ToolOutcome::Success {
            warnings.push(source.unavailable_warning());
            continue;
        }
        let Some(content) = source.content(&result.content) else {
            warnings.push(source.invalid_warning());
            continue;
        };
        if source
            .raw_content_chars(&result.content)
            .unwrap_or_default()
            > PROJECT_TOOL_CONTEXT_CHARS
        {
            warnings.push(source.limit_warning());
            continue;
        }
        if added.insert(source.native_name()) {
            chunks.push(source.chunk(content));
        }
    }
}

pub(crate) fn render_project_help_answer(answer: &str) -> Result<String> {
    let chars = answer.chars().collect::<Vec<_>>();
    let mut clean = String::with_capacity(answer.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '[' {
            let mut end = index + 1;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            if end > index + 1 && end < chars.len() && chars[end] == ']' {
                index = end + 1;
                continue;
            }
        }
        clean.push(chars[index]);
        index += 1;
    }
    let mut visible_lines = Vec::new();
    for line in clean.lines() {
        let heading = line.trim().trim_matches('#').trim().to_lowercase();
        if ["источники", "цитаты", "sources", "citations"]
            .iter()
            .any(|label| {
                heading == *label
                    || heading
                        .strip_prefix(label)
                        .is_some_and(|suffix| suffix.trim_start().starts_with(':'))
            })
        {
            break;
        }
        visible_lines.push(line.trim_end());
    }
    let mut clean = visible_lines.join("\n").trim().to_owned();
    for (spaced, punctuation) in [(" .", "."), (" ,", ","), (" :", ":"), (" ;", ";")] {
        clean = clean.replace(spaced, punctuation);
    }
    clean = redact_mcp_uris(&clean);
    clean = replace_ascii_case_insensitive(&clean, "chunk_id", "идентификатор фрагмента");
    clean = replace_ascii_case_insensitive(&clean, "tool trace", "служебная трассировка");
    anyhow::ensure!(!clean.is_empty(), "чистый ответ project help пуст");
    let lower = clean.to_lowercase();
    for forbidden in ["<rag_attribution>", "</rag_attribution>"] {
        anyhow::ensure!(
            !lower.contains(forbidden),
            "ответ project help содержит служебную атрибуцию"
        );
    }
    Ok(clean)
}

fn redact_mcp_uris(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut remainder = text;
    loop {
        let lowercase = remainder.to_ascii_lowercase();
        let Some(start) = lowercase.find("mcp://") else {
            clean.push_str(remainder);
            break;
        };
        clean.push_str(&remainder[..start]);
        clean.push_str("MCP-инструмент");
        let uri = &remainder[start..];
        let end = uri
            .char_indices()
            .find_map(|(index, character)| {
                (!character.is_ascii_alphanumeric()
                    && !matches!(character, ':' | '/' | '_' | '-' | '.'))
                .then_some(index)
            })
            .unwrap_or(uri.len());
        remainder = &uri[end..];
    }
    clean
}

fn replace_ascii_case_insensitive(text: &str, needle: &str, replacement: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut remainder = text;
    loop {
        let lowercase = remainder.to_ascii_lowercase();
        let Some(start) = lowercase.find(needle) else {
            clean.push_str(remainder);
            break;
        };
        clean.push_str(&remainder[..start]);
        clean.push_str(replacement);
        remainder = &remainder[start + needle.len()..];
    }
    clean
}

#[derive(Debug, Clone, Copy)]
enum ProjectToolSource {
    GitBranch,
    CodeGraph,
}

impl ProjectToolSource {
    fn from_tool_name(name: &str) -> Option<Self> {
        if name == "git_current_branch" || name.ends_with("__git_current_branch") {
            Some(Self::GitBranch)
        } else if name == "codegraph_explore" || name.ends_with("__codegraph_explore") {
            Some(Self::CodeGraph)
        } else {
            None
        }
    }
    fn native_name(self) -> &'static str {
        match self {
            Self::GitBranch => "git_current_branch",
            Self::CodeGraph => "codegraph_explore",
        }
    }

    fn content(self, value: &Value) -> Option<String> {
        match self {
            Self::GitBranch => match (
                value.get("branch").and_then(Value::as_str),
                value.get("detached").and_then(Value::as_bool),
            ) {
                (Some(branch), Some(false)) if !branch.trim().is_empty() => {
                    Some(format!("Текущая Git-ветка: {branch}"))
                }
                (None, Some(true)) => Some("Текущая Git-ветка: detached HEAD".into()),
                _ => None,
            },
            Self::CodeGraph => value
                .get("output")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|output| !output.is_empty())
                .map(str::to_owned),
        }
    }

    fn raw_content_chars(self, value: &Value) -> Option<usize> {
        match self {
            Self::GitBranch => {
                if let Some(branch) = value.get("branch").and_then(Value::as_str) {
                    Some(branch.chars().count())
                } else if value.get("detached").and_then(Value::as_bool) == Some(true) {
                    Some(0)
                } else {
                    None
                }
            }
            Self::CodeGraph => value
                .get("output")
                .and_then(Value::as_str)
                .map(|output| output.chars().count()),
        }
    }

    fn chunk(self, content: String) -> RetrievedChunk {
        let (source, title, section) = match self {
            Self::GitBranch => (
                "mcp://workspace/git_current_branch",
                "Git workspace",
                "current branch",
            ),
            Self::CodeGraph => (
                "mcp://workspace/codegraph_explore",
                "CodeGraph",
                "code structure",
            ),
        };
        RetrievedChunk {
            chunk_id: format!("project-help-{}", self.native_name()),
            source: source.into(),
            title: title.into(),
            section: section.into(),
            content,
            similarity: 1.0,
            original_similarity: 1.0,
            rewritten_similarity: None,
            rerank_score: Some(1.0),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::GitBranch => "Git-ветка",
            Self::CodeGraph => "CodeGraph",
        }
    }

    fn unavailable_warning(self) -> String {
        format!("{} недоступен через workspace MCP.", self.label())
    }

    fn invalid_warning(self) -> String {
        format!("{} вернул некорректный результат.", self.label())
    }

    fn limit_warning(self) -> String {
        format!("{} превысил лимит проектного контекста.", self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentSettings, ApiAnswer, Message, RequestClient, RequestFuture, RequestOptions,
        ToolDefinition, ToolFuture,
    };
    use reqwest::Client;
    use serde_json::json;
    use std::{collections::VecDeque, sync::Mutex};

    struct ScriptedHelpClient {
        answers: Mutex<VecDeque<ApiAnswer>>,
        requests: Mutex<Vec<(Vec<Message>, RequestOptions)>>,
    }

    impl RequestClient for ScriptedHelpClient {
        fn send<'a>(
            &'a self,
            _client: &'a Client,
            _settings: &'a AgentSettings,
            history: &'a [Message],
        ) -> RequestFuture<'a> {
            self.requests
                .lock()
                .unwrap()
                .push((history.to_vec(), RequestOptions::default()));
            let answer = self.answers.lock().unwrap().pop_front().unwrap();
            Box::pin(async move { Ok(answer) })
        }

        fn send_with_options<'a>(
            &'a self,
            _client: &'a Client,
            _settings: &'a AgentSettings,
            history: &'a [Message],
            options: &'a RequestOptions,
        ) -> RequestFuture<'a> {
            self.requests
                .lock()
                .unwrap()
                .push((history.to_vec(), options.clone()));
            let answer = self.answers.lock().unwrap().pop_front().unwrap();
            Box::pin(async move { Ok(answer) })
        }
    }

    struct HelpExecutor;

    impl crate::ToolExecutor for HelpExecutor {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![
                ToolDefinition {
                    name: "project-help__git_current_branch".into(),
                    description: None,
                    input_schema: json!({"type": "object"}),
                    read_only: true,
                    destructive: false,
                },
                ToolDefinition {
                    name: "project-help__codegraph_explore".into(),
                    description: None,
                    input_schema: json!({
                        "type": "object",
                        "properties": {"question": {"type": "string"}},
                        "required": ["question"]
                    }),
                    read_only: true,
                    destructive: false,
                },
            ]
        }

        fn execute<'a>(&'a self, call: &'a ToolCall) -> ToolFuture<'a> {
            let result = ToolResult {
                call_id: call.id.clone(),
                content: json!({"output": "src/app.rs вызывает изолированный project help"}),
                is_error: false,
                outcome: ToolOutcome::Success,
            };
            Box::pin(async move { Ok(result) })
        }
    }

    fn answer(text: &str, tool_calls: Vec<ToolCall>) -> ApiAnswer {
        ApiAnswer {
            text: text.into(),
            task_update: None,
            task_update_warning: None,
            input_tokens: 10,
            output_tokens: 5,
            session_input_tokens: 0,
            session_output_tokens: 0,
            tool_calls,
            rag_citations: Vec::new(),
            generation_requests: 1,
            repair_requests: 0,
        }
    }

    fn document_chunk() -> RetrievedChunk {
        RetrievedChunk {
            chunk_id: "doc-1".into(),
            source: "README.md".into(),
            title: "Project".into(),
            section: "Help".into(),
            content: "Project help использует отдельного агента".into(),
            similarity: 0.9,
            original_similarity: 0.9,
            rewritten_similarity: Some(0.9),
            rerank_score: Some(0.9),
        }
    }

    fn grounded_answer(statement: &str) -> String {
        format!(
            "{statement} [1].\n<RAG_ATTRIBUTION>\ncitations[1]{{id,context_id,source,section,chunk_id,quote}}:\n  1,2,\"mcp://workspace/codegraph_explore\",\"code structure\",\"project-help-codegraph_explore\",\"src/app.rs вызывает изолированный project help\"\n</RAG_ATTRIBUTION>"
        )
    }

    #[test]
    fn help_command_distinguishes_commands_questions_and_other_input() {
        assert_eq!(parse_help_command("/help"), HelpCommand::ShowCommands);
        assert_eq!(parse_help_command("/help   "), HelpCommand::ShowCommands);
        assert_eq!(
            parse_help_command("/help   как устроен MCP?"),
            HelpCommand::Ask("как устроен MCP?")
        );
        assert_eq!(parse_help_command("/helper"), HelpCommand::NotHelp);
    }

    #[test]
    fn clean_help_answer_removes_numeric_markers_and_sanitizes_service_data() {
        assert_eq!(
            render_project_help_answer("Модули связаны так [1]. Затем вызывается MCP [2].")
                .unwrap(),
            "Модули связаны так. Затем вызывается MCP."
        );
        assert_eq!(
            render_project_help_answer("Функция объединяет источники: документацию и MCP [1].")
                .unwrap(),
            "Функция объединяет источники: документацию и MCP."
        );
        assert_eq!(
            render_project_help_answer("Ответ подтверждён [1].\n\n## Источники\nREADME.md")
                .unwrap(),
            "Ответ подтверждён."
        );
        assert_eq!(
            render_project_help_answer("Ответ подтверждён [1].\nЦитаты: точный фрагмент").unwrap(),
            "Ответ подтверждён."
        );
        let sanitized = render_project_help_answer(
            "MCP://workspace/tool вернул chunk_id; tool trace скрыт [1].",
        )
        .unwrap();
        assert!(!sanitized.to_lowercase().contains("mcp://"));
        assert!(!sanitized.to_lowercase().contains("chunk_id"));
        assert!(!sanitized.to_lowercase().contains("tool trace"));
        assert!(render_project_help_answer("<RAG_ATTRIBUTION>").is_err());
    }

    #[tokio::test]
    async fn subagent_is_fresh_uses_only_help_tools_and_returns_clean_answer() {
        let first = "Как устроен help?";
        let second = "Где находится обработчик?";
        let client = Arc::new(ScriptedHelpClient {
            answers: Mutex::new(VecDeque::from([
                answer(
                    "",
                    vec![ToolCall {
                        id: "tool-1".into(),
                        name: "project-help__codegraph_explore".into(),
                        arguments: json!({"question": first}),
                    }],
                ),
                answer("Контекст собран", Vec::new()),
                answer(&grounded_answer("Help изолирован"), Vec::new()),
                answer(
                    "",
                    vec![ToolCall {
                        id: "tool-2".into(),
                        name: "project-help__codegraph_explore".into(),
                        arguments: json!({"question": second}),
                    }],
                ),
                answer("Контекст собран", Vec::new()),
                answer(&grounded_answer("Обработчик найден"), Vec::new()),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let mut parent = Agent::new(
            1,
            Client::new(),
            AgentSettings {
                provider: crate::Provider::Ollama,
                api_key: None,
                endpoint: None,
                model: "test".into(),
                temperature: 0.0,
                instructions: Some("родительский режим".into()),
                compression_strategy: crate::CompressionStrategy::Summary,
                context_messages: 10,
            },
        );
        parent.request_client = client.clone();
        parent.history.push(Message {
            role: "user".into(),
            content: "Секрет предыдущего диалога".into(),
        });
        parent.summary = "Секретный summary".into();
        let executor: SharedToolExecutor = Arc::new(HelpExecutor);

        let first_result = run_project_help_subagent(
            &parent,
            Some(executor.clone()),
            first,
            vec![document_chunk()],
        )
        .await
        .unwrap();
        let second_result =
            run_project_help_subagent(&parent, Some(executor), second, vec![document_chunk()])
                .await
                .unwrap();

        assert_eq!(first_result.answer.unwrap().text, "Help изолирован.");
        let second_answer = second_result.answer.unwrap();
        assert_eq!(second_answer.text, "Обработчик найден.");
        assert!(!second_answer.rag_citations.is_empty());
        assert!(!second_answer.text.contains("Источники"));
        assert!(!second_answer.text.contains("mcp://"));

        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 6);
        for index in [0usize, 3] {
            let (history, options) = &requests[index];
            assert_eq!(history.len(), 1);
            let text = &history[0].content;
            assert!(!text.contains("Секрет предыдущего диалога"));
            assert!(!text.contains("Секретный summary"));
            assert_eq!(options.tools.len(), 2);
            assert!(options
                .tools
                .iter()
                .all(|tool| tool.name != crate::PIPELINE_TOOL_NAME));
        }
        assert!(!requests[3].0[0].content.contains(first));
        assert!(requests[3].0[0].content.contains(second));
    }
}
