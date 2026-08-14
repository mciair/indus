//! Durable in-memory session state projected from harness stream events.

use serde::{Deserialize, Serialize};

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
    pub messages: Vec<SessionMessage>,
    #[serde(default)]
    next_message_id: u64,
}

impl Session {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            messages: Vec::new(),
            next_message_id: 0,
        }
    }

    pub fn push_user(&mut self, text: impl Into<String>) -> u64 {
        let id = self.allocate_message_id();
        self.messages.push(SessionMessage::User(UserMessage {
            id,
            text: text.into(),
        }));
        id
    }

    pub fn next_assistant(&mut self, parent_id: u64) -> AssistantMessage {
        AssistantMessage::new(self.allocate_message_id(), parent_id)
    }

    pub fn push_assistant(&mut self, message: AssistantMessage) {
        self.messages.push(SessionMessage::Assistant(message));
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
}

impl Default for Session {
    fn default() -> Self {
        Self::new("default")
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
}
