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
            messages,
            next_message_id,
        }
    }

    pub fn is_allocated(&self) -> bool {
        self.id.starts_with("ses-i_")
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
        if self.is_allocated() {
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
        self.messages
            .iter()
            .rev()
            .find_map(|message| match message {
                SessionMessage::Assistant(assistant) => Some(assistant.usage.context_tokens),
                SessionMessage::User(_) => None,
            })
            .unwrap_or(0)
    }

    pub fn summary_source(&self, max_characters: usize) -> String {
        let mut output = String::new();
        for message in &self.messages {
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

    pub fn compact(&mut self, summary: impl Into<String>, preserve_messages: usize) {
        let keep_from = self.messages.len().saturating_sub(preserve_messages);
        let preserved = self.messages.split_off(keep_from);
        self.messages.clear();
        let id = self.allocate_message_id();
        self.messages.push(SessionMessage::User(UserMessage {
            id,
            text: format!("[Conversation summary]\n{}", summary.into().trim()),
        }));
        self.messages.extend(preserved);
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
}
