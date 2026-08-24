//! Durable in-memory session state projected from harness stream events.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    event::FileDiff,
    model::{ModelContent, ModelMessage, Role, StopReason, Usage},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    pub id: u64,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReasoningPart {
    pub id: String,
    pub text: String,
    pub completed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TextPart {
    pub id: String,
    pub text: String,
    pub completed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ToolState {
    Pending,
    Running,
    Completed {
        title: String,
        output: String,
        diffs: Vec<FileDiff>,
    },
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolPart {
    pub call_id: String,
    pub name: String,
    pub input: String,
    pub state: ToolState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AssistantPart {
    Reasoning(ReasoningPart),
    Text(TextPart),
    Tool(ToolPart),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub id: u64,
    pub parent_id: u64,
    pub parts: Vec<AssistantPart>,
    pub finish: Option<StopReason>,
    pub usage: Usage,
    pub error: Option<String>,
}

impl AssistantMessage {
    pub fn new(id: u64, parent_id: u64) -> Self {
        Self {
            id,
            parent_id,
            parts: Vec::new(),
            finish: None,
            usage: Usage::default(),
            error: None,
        }
    }

    pub fn has_tool_calls(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, AssistantPart::Tool(_)))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SessionMessage {
    User(UserMessage),
    Assistant(AssistantMessage),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactionInput {
    pub previous_summary: Option<String>,
    pub history: String,
    pub preserve_from: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub directory: String,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub ephemeral: bool,
    pub messages: Vec<SessionMessage>,
    #[serde(default)]
    next_message_id: u64,
}

impl Session {
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        let timestamp = if id.is_empty() { 0 } else { now_ms() };
        Self {
            id,
            title: None,
            directory: String::new(),
            provider_id: None,
            model_id: None,
            created_at: timestamp,
            updated_at: timestamp,
            ephemeral: false,
            messages: Vec::new(),
            next_message_id: 0,
        }
    }

    pub fn unallocated(directory: impl Into<String>) -> Self {
        let mut session = Self::new("");
        session.directory = directory.into();
        session
    }

    #[allow(clippy::too_many_arguments)]
    pub fn restore(
        id: String,
        title: String,
        directory: String,
        provider_id: Option<String>,
        model_id: Option<String>,
        created_at: i64,
        updated_at: i64,
        messages: Vec<SessionMessage>,
    ) -> Self {
        let next_message_id = messages
            .iter()
            .map(|message| match message {
                SessionMessage::User(message) => message.id,
                SessionMessage::Assistant(message) => message.id,
            })
            .max()
            .unwrap_or(0);
        Self {
            id,
            title: Some(title),
            directory,
            provider_id,
            model_id,
            created_at,
            updated_at,
            ephemeral: false,
            messages,
            next_message_id,
        }
    }

    pub fn is_allocated(&self) -> bool {
        !self.ephemeral
            && self.id.starts_with("ses-i_")
            && self
                .title
                .as_deref()
                .is_some_and(|title| !title.trim().is_empty())
    }

    pub fn allocate(
        &mut self,
        id: impl Into<String>,
        title: impl Into<String>,
        provider_id: Option<String>,
        model_id: Option<String>,
    ) -> bool {
        if self.is_allocated() || self.ephemeral {
            return false;
        }
        let title = title.into().trim().to_string();
        let id = id.into();
        if title.is_empty() || !id.starts_with("ses-i_") {
            return false;
        }
        let timestamp = now_ms();
        self.id = id;
        self.title = Some(title);
        self.provider_id = provider_id;
        self.model_id = model_id;
        self.created_at = timestamp;
        self.updated_at = timestamp;
        true
    }

    pub fn ephemeral_fork(&self) -> Self {
        let mut fork = self.clone();
        fork.id.clear();
        fork.title = Some(match self.title.as_deref() {
            Some(title) => format!("Fork of {title}"),
            None => "Ephemeral fork".to_string(),
        });
        fork.created_at = 0;
        fork.updated_at = 0;
        fork.ephemeral = true;
        fork
    }

    pub fn rename(&mut self, title: impl Into<String>) -> bool {
        if !self.is_allocated() {
            return false;
        }
        let title = title.into().trim().to_string();
        if title.is_empty() {
            return false;
        }
        self.title = Some(title);
        self.touch();
        true
    }

    pub fn rewind_last_turn(&mut self) -> Option<String> {
        let index = self
            .messages
            .iter()
            .rposition(|message| matches!(message, SessionMessage::User(_)))?;
        let SessionMessage::User(message) = &self.messages[index] else {
            return None;
        };
        let prompt = message.text.clone();
        self.messages.truncate(index);
        self.touch();
        Some(prompt)
    }

    pub fn push_user(&mut self, text: impl Into<String>) -> u64 {
        let id = self.allocate_message_id();
        self.messages.push(SessionMessage::User(UserMessage {
            id,
            text: text.into(),
        }));
        self.touch();
        id
    }

    pub fn next_assistant(&mut self, parent_id: u64) -> AssistantMessage {
        AssistantMessage::new(self.allocate_message_id(), parent_id)
    }

    pub fn push_assistant(&mut self, message: AssistantMessage) {
        self.messages.push(SessionMessage::Assistant(message));
        self.touch();
    }

    pub fn model_messages(&self) -> Vec<ModelMessage> {
        let mut output = Vec::new();
        for message in &self.messages {
            match message {
                SessionMessage::User(user) => output.push(ModelMessage {
                    role: Role::User,
                    content: vec![ModelContent::Text(user.text.clone())],
                }),
                SessionMessage::Assistant(assistant) => {
                    let content = assistant
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            AssistantPart::Text(text) if !text.text.is_empty() => {
                                Some(ModelContent::Text(text.text.clone()))
                            }
                            AssistantPart::Tool(tool) => Some(ModelContent::ToolCall {
                                call_id: tool.call_id.clone(),
                                name: tool.name.clone(),
                                input: tool.input.clone(),
                            }),
                            AssistantPart::Reasoning(_) | AssistantPart::Text(_) => None,
                        })
                        .collect();
                    output.push(ModelMessage {
                        role: Role::Assistant,
                        content,
                    });
                    for part in &assistant.parts {
                        let AssistantPart::Tool(tool) = part else {
                            continue;
                        };
                        let (result, is_error) = match &tool.state {
                            ToolState::Completed { output, .. } => (Some(output.clone()), false),
                            ToolState::Failed { message } => (Some(message.clone()), true),
                            ToolState::Pending | ToolState::Running => (None, false),
                        };
                        if let Some(result) = result {
                            output.push(ModelMessage {
                                role: Role::Tool,
                                content: vec![ModelContent::ToolResult {
                                    call_id: tool.call_id.clone(),
                                    name: tool.name.clone(),
                                    output: result,
                                    is_error,
                                }],
                            });
                        }
                    }
                }
            }
        }
        output
    }

    pub fn repeated_tool_call_count(&self, name: &str, input: &str) -> usize {
        let mut count = 0;
        for message in self.messages.iter().rev() {
            let SessionMessage::Assistant(assistant) = message else {
                continue;
            };
            for part in assistant.parts.iter().rev() {
                match part {
                    AssistantPart::Tool(tool) if tool.name == name && tool.input == input => {
                        count += 1;
                    }
                    _ => return count,
                }
            }
        }
        count
    }

    pub fn current_context_tokens(&self) -> u64 {
        let measured = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, message)| match message {
                SessionMessage::Assistant(assistant) if assistant.usage.context_tokens > 0 => {
                    Some((index, assistant.usage.context_tokens))
                }
                SessionMessage::Assistant(_) | SessionMessage::User(_) => None,
            });

        // A provider only reports occupancy for a turn it has already served.
        // Everything appended since that reading - the pending prompt plus every
        // tool result gathered during this run - still travels in the next
        // request, so it has to be estimated or the meter reads low at exactly
        // the moment the threshold matters.
        let Some((measured_index, measured_tokens)) = measured else {
            return estimated_tokens(&self.messages);
        };
        measured_tokens.saturating_add(estimated_tokens(&self.messages[measured_index + 1..]))
    }

    pub fn summary_source(&self, max_characters: usize) -> String {
        summary_source(&self.messages, max_characters)
    }

    pub fn compaction_input(
        &self,
        preserve_user_turns: usize,
        max_characters: usize,
    ) -> Option<CompactionInput> {
        let user_indices = self
            .messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| match message {
                SessionMessage::User(user) if !is_summary(&user.text) => Some(index),
                SessionMessage::User(_) | SessionMessage::Assistant(_) => None,
            })
            .collect::<Vec<_>>();
        if user_indices.is_empty() {
            return None;
        }

        // Keep the requested recent turns verbatim while ensuring there is
        // always older material for a manual first-turn compaction to summarize.
        let preserved_turns = preserve_user_turns.min(user_indices.len().saturating_sub(1));
        let preserve_from = if preserved_turns == 0 {
            self.messages.len()
        } else {
            user_indices[user_indices.len() - preserved_turns]
        };
        let previous_summary =
            self.messages[..preserve_from]
                .iter()
                .find_map(|message| match message {
                    SessionMessage::User(user) => user
                        .text
                        .strip_prefix("[Conversation summary]\n")
                        .map(str::trim)
                        .filter(|summary| !summary.is_empty())
                        .map(str::to_string),
                    SessionMessage::Assistant(_) => None,
                });
        let head = self.messages[..preserve_from]
            .iter()
            .filter(|message| match message {
                SessionMessage::User(user) => !is_summary(&user.text),
                SessionMessage::Assistant(_) => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        let history = summary_source(&head, max_characters);
        if history.trim().is_empty() {
            return None;
        }
        Some(CompactionInput {
            previous_summary,
            history,
            preserve_from,
        })
    }

    pub fn compact_at(&mut self, summary: impl Into<String>, preserve_from: usize) {
        let preserve_from = preserve_from.min(self.messages.len());
        let mut preserved = self.messages.split_off(preserve_from);
        // The retained turns still carry the occupancy the provider measured
        // before the transcript shrank. Leaving those readings in place would
        // keep the context meter pinned above the threshold and compact again
        // on every following step, so the estimate takes over until the next
        // request comes back with a fresh measurement.
        for message in &mut preserved {
            if let SessionMessage::Assistant(assistant) = message {
                assistant.usage.context_tokens = 0;
            }
        }
        self.messages.clear();
        let id = self.allocate_message_id();
        self.messages.push(SessionMessage::User(UserMessage {
            id,
            text: format!("[Conversation summary]\n{}", summary.into().trim()),
        }));
        self.messages.extend(preserved);
        self.touch();
    }

    pub fn compact(&mut self, summary: impl Into<String>, preserve_messages: usize) {
        let preserve_from = self.messages.len().saturating_sub(preserve_messages);
        self.compact_at(summary, preserve_from);
    }

    fn allocate_message_id(&mut self) -> u64 {
        self.next_message_id = self.next_message_id.saturating_add(1);
        self.next_message_id
    }

    fn touch(&mut self) {
        if self.is_allocated() {
            self.updated_at = now_ms();
        }
    }
}

pub fn title_from_first_prompt(prompt: &str) -> Option<String> {
    let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.chars().take(100).collect())
}

fn is_summary(text: &str) -> bool {
    text.starts_with("[Conversation summary]\n")
}

/// Average characters per token across the transcripts these providers bill.
const CHARACTERS_PER_TOKEN: u64 = 4;
/// Per-message envelope: role markers, tool-call scaffolding and delimiters.
const MESSAGE_OVERHEAD_TOKENS: u64 = 4;

/// Approximates how many tokens `messages` will occupy in the next request.
/// This is deliberately a local estimate: it has to be available before a
/// request is sent, which is the only point where compaction can still keep the
/// conversation inside the window.
fn estimated_tokens(messages: &[SessionMessage]) -> u64 {
    messages
        .iter()
        .map(|message| {
            let characters = match message {
                SessionMessage::User(user) => user.text.chars().count() as u64,
                SessionMessage::Assistant(assistant) => assistant
                    .parts
                    .iter()
                    .map(|part| match part {
                        AssistantPart::Text(text) => text.text.chars().count() as u64,
                        // Reasoning is not replayed to the provider, so it is
                        // excluded from the projection.
                        AssistantPart::Reasoning(_) => 0,
                        AssistantPart::Tool(tool) => {
                            let result = match &tool.state {
                                ToolState::Completed { output, .. } => output.chars().count(),
                                ToolState::Failed { message } => message.chars().count(),
                                ToolState::Pending | ToolState::Running => 0,
                            };
                            (tool.name.chars().count() + tool.input.chars().count() + result) as u64
                        }
                    })
                    .sum(),
            };
            characters
                .div_ceil(CHARACTERS_PER_TOKEN)
                .saturating_add(MESSAGE_OVERHEAD_TOKENS)
        })
        .sum()
}

fn summary_source(messages: &[SessionMessage], max_characters: usize) -> String {
    let mut output = String::new();
    for message in messages {
        let block = match message {
            SessionMessage::User(user) => format!("User:\n{}\n\n", user.text),
            SessionMessage::Assistant(assistant) => {
                let parts = assistant
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        AssistantPart::Text(text) => Some(text.text.clone()),
                        AssistantPart::Tool(tool) => {
                            let result = match &tool.state {
                                ToolState::Completed { output, .. } => output.as_str(),
                                ToolState::Failed { message } => message.as_str(),
                                ToolState::Pending | ToolState::Running => "incomplete",
                            };
                            Some(format!("Tool {}({}) => {result}", tool.name, tool.input))
                        }
                        AssistantPart::Reasoning(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("Assistant:\n{parts}\n\n")
            }
        };
        output.push_str(&block);
        if output.len() > max_characters {
            let mut start = output.len().saturating_sub(max_characters);
            while !output.is_char_boundary(start) {
                start += 1;
            }
            output = format!("[Earlier transcript omitted]\n{}", &output[start..]);
        }
    }
    output
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

impl Default for Session {
    fn default() -> Self {
        Self::unallocated("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_follow_the_assistant_tool_call() {
        let mut session = Session::default();
        let user_id = session.push_user("inspect the repository");
        let mut assistant = session.next_assistant(user_id);
        assistant.parts.push(AssistantPart::Tool(ToolPart {
            call_id: "call-1".into(),
            name: "shell".into(),
            input: "git status".into(),
            state: ToolState::Completed {
                title: "Check repository status".into(),
                output: "clean".into(),
                diffs: Vec::new(),
            },
        }));
        session.push_assistant(assistant);

        let messages = session.model_messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[2].role, Role::Tool);
    }

    #[test]
    fn message_identifiers_increase_within_a_session() {
        let mut session = Session::default();
        let first = session.push_user("first");
        let second = session.next_assistant(first).id;
        assert!(second > first);
    }

    #[test]
    fn context_occupancy_uses_the_latest_request_instead_of_cumulative_usage() {
        let mut session = Session::default();
        let first_user = session.push_user("first");
        let mut first_assistant = session.next_assistant(first_user);
        first_assistant.usage.context_tokens = 70_000;
        session.push_assistant(first_assistant);
        let second_user = session.push_user("second");
        let mut second_assistant = session.next_assistant(second_user);
        second_assistant.usage.context_tokens = 80_000;
        session.push_assistant(second_assistant);

        assert_eq!(session.current_context_tokens(), 80_000);
    }

    #[test]
    fn context_occupancy_projects_material_added_since_the_last_measurement() {
        let mut session = Session::default();
        let user = session.push_user("first");
        let mut assistant = session.next_assistant(user);
        assistant.usage.context_tokens = 80_000;
        session.push_assistant(assistant);
        let measured = session.current_context_tokens();

        session.push_user("x".repeat(4_000).as_str());
        let projected = session.current_context_tokens();

        assert_eq!(measured, 80_000);
        assert!(
            projected >= 81_000,
            "the pending prompt has to raise occupancy, got {projected}"
        );
    }

    #[test]
    fn compaction_releases_the_occupancy_measured_before_the_transcript_shrank() {
        let mut session = Session::default();
        for index in 0..3 {
            let user = session.push_user(format!("turn {index}"));
            let mut assistant = session.next_assistant(user);
            assistant.parts.push(AssistantPart::Text(TextPart {
                id: format!("text-{index}"),
                text: "y".repeat(200),
                completed: true,
            }));
            assistant.usage.context_tokens = 180_000;
            session.push_assistant(assistant);
        }
        assert_eq!(session.current_context_tokens(), 180_000);

        let input = session.compaction_input(2, 90_000).unwrap();
        session.compact_at("short summary", input.preserve_from);

        assert!(
            session.current_context_tokens() < 1_000,
            "a compacted transcript must not keep reporting pre-compaction occupancy, got {}",
            session.current_context_tokens()
        );
    }

    #[test]
    fn restored_sessions_continue_message_identifiers() {
        let messages = vec![SessionMessage::User(UserMessage {
            id: 7,
            text: "existing".into(),
        })];
        let mut session = Session::restore(
            "ses-i_example".into(),
            "Existing session".into(),
            "/tmp".into(),
            None,
            None,
            1,
            2,
            messages,
        );

        assert_eq!(session.push_user("next"), 8);
    }

    #[test]
    fn rewinding_removes_the_latest_turn_and_preserves_earlier_history() {
        let mut session = Session::unallocated("/workspace");
        let first = session.push_user("first");
        let first_answer = session.next_assistant(first);
        session.push_assistant(first_answer);
        session.push_user("edit this prompt");

        assert_eq!(
            session.rewind_last_turn().as_deref(),
            Some("edit this prompt")
        );
        assert_eq!(session.messages.len(), 2);
    }

    #[test]
    fn only_allocated_sessions_can_be_renamed() {
        let mut session = Session::unallocated("/workspace");
        assert!(!session.rename("Not persisted"));
        assert!(session.allocate("ses-i_example", "Initial", None, None));
        assert!(session.rename("Renamed Session"));
        assert_eq!(session.title.as_deref(), Some("Renamed Session"));
    }

    #[test]
    fn ephemeral_forks_clone_history_without_becoming_persistable() {
        let mut session = Session::unallocated("/workspace");
        assert!(session.allocate("ses-i_parent", "Parent", None, None));
        session.push_user("preserved history");

        let mut fork = session.ephemeral_fork();
        assert!(fork.ephemeral);
        assert!(!fork.is_allocated());
        assert_eq!(fork.title.as_deref(), Some("Fork of Parent"));
        assert_eq!(fork.messages, session.messages);
        assert!(!fork.allocate("ses-i_fork", "Fork", None, None));
    }

    #[test]
    fn first_prompt_becomes_the_session_title() {
        assert_eq!(
            title_from_first_prompt("  Build   an Indus\nCLI  ").as_deref(),
            Some("Build an Indus CLI")
        );
        assert_eq!(title_from_first_prompt(" \n\t "), None);
        assert_eq!(
            title_from_first_prompt(&"a".repeat(120))
                .unwrap()
                .chars()
                .count(),
            100
        );
    }

    #[test]
    fn compaction_preserves_the_two_newest_user_turns() {
        let mut session = Session::unallocated("/workspace");
        for prompt in ["first", "second", "third"] {
            let parent = session.push_user(prompt);
            let mut answer = session.next_assistant(parent);
            answer.parts.push(AssistantPart::Text(TextPart {
                id: format!("answer-{prompt}"),
                text: format!("answer to {prompt}"),
                completed: true,
            }));
            session.push_assistant(answer);
        }

        let input = session.compaction_input(2, 90_000).unwrap();
        assert!(input.history.contains("first"));
        assert!(!input.history.contains("second"));
        session.compact_at("anchored summary", input.preserve_from);

        assert_eq!(session.messages.len(), 5);
        assert!(matches!(
            &session.messages[0],
            SessionMessage::User(message) if message.text.contains("anchored summary")
        ));
        assert!(matches!(
            &session.messages[1],
            SessionMessage::User(message) if message.text == "second"
        ));
    }

    #[test]
    fn subsequent_compaction_extracts_the_previous_anchor() {
        let mut session = Session::unallocated("/workspace");
        session.push_user("[Conversation summary]\n## Goal\n- Keep this");
        for prompt in ["second", "third", "fourth"] {
            let parent = session.push_user(prompt);
            let answer = session.next_assistant(parent);
            session.push_assistant(answer);
        }

        let input = session.compaction_input(2, 90_000).unwrap();
        assert_eq!(
            input.previous_summary.as_deref(),
            Some("## Goal\n- Keep this")
        );
        assert!(input.history.contains("second"));
        assert!(!input.history.contains("[Conversation summary]"));
    }
}
