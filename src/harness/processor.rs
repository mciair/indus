//! Projects normalized model events into session message parts.

use std::collections::HashMap;

use super::{
    event::HarnessEvent,
    model::{ModelEvent, StopReason},
    session::{AssistantMessage, AssistantPart, ReasoningPart, TextPart, ToolPart, ToolState},
    tool::ToolOutput,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessOutcome {
    Continue,
    Stop,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingToolCall {
    pub call_id: String,
    pub name: String,
    pub input: String,
}

pub struct StreamProcessor {
    run_id: u64,
    message: AssistantMessage,
    reasoning: HashMap<String, usize>,
    text: HashMap<String, usize>,
    tools: HashMap<String, usize>,
    pending_tools: Vec<PendingToolCall>,
}

impl StreamProcessor {
    pub fn new(run_id: u64, message: AssistantMessage) -> Self {
        Self {
            run_id,
            message,
            reasoning: HashMap::new(),
            text: HashMap::new(),
            tools: HashMap::new(),
            pending_tools: Vec::new(),
        }
    }

    pub fn handle(&mut self, event: ModelEvent, emit: &dyn Fn(HarnessEvent)) {
        match event {
            ModelEvent::StepStarted => {}
            ModelEvent::ReasoningStarted { id } => {
                if self.reasoning.contains_key(&id) {
                    return;
                }
                let index = self.message.parts.len();
                self.message
                    .parts
                    .push(AssistantPart::Reasoning(ReasoningPart {
                        id: id.clone(),
                        text: String::new(),
                        completed: false,
                    }));
                self.reasoning.insert(id.clone(), index);
                emit(HarnessEvent::ReasoningStarted {
                    run_id: self.run_id,
                    reasoning_id: id,
                });
            }
            ModelEvent::ReasoningDelta { id, text } => {
                let Some(index) = self.reasoning.get(&id).copied() else {
                    return;
                };
                if let Some(AssistantPart::Reasoning(part)) = self.message.parts.get_mut(index) {
                    part.text.push_str(&text);
                }
                emit(HarnessEvent::ReasoningDelta {
                    run_id: self.run_id,
                    reasoning_id: id,
                    text,
                });
            }
            ModelEvent::ReasoningFinished { id } => self.finish_reasoning(&id, emit),
            ModelEvent::TextStarted { id } => {
                if self.text.contains_key(&id) {
                    return;
                }
                let index = self.message.parts.len();
                self.message.parts.push(AssistantPart::Text(TextPart {
                    id: id.clone(),
                    text: String::new(),
                    completed: false,
                }));
                self.text.insert(id.clone(), index);
                emit(HarnessEvent::TextStarted {
                    run_id: self.run_id,
                    text_id: id,
                });
            }
            ModelEvent::TextDelta { id, text } => {
                let Some(index) = self.text.get(&id).copied() else {
                    return;
                };
                if let Some(AssistantPart::Text(part)) = self.message.parts.get_mut(index) {
                    part.text.push_str(&text);
                }
                emit(HarnessEvent::TextDelta {
                    run_id: self.run_id,
                    text_id: id,
                    text,
                });
            }
            ModelEvent::TextFinished { id } => self.finish_text(&id, emit),
            ModelEvent::ToolInputStarted { id, name } => {
                self.ensure_tool(&id, &name);
            }
            ModelEvent::ToolInputDelta { id, text } => {
                let Some(index) = self.tools.get(&id).copied() else {
                    return;
                };
                if let Some(AssistantPart::Tool(part)) = self.message.parts.get_mut(index) {
                    part.input.push_str(&text);
                }
            }
            ModelEvent::ToolCall { id, name, input } => {
                let index = self.ensure_tool(&id, &name);
                if let Some(AssistantPart::Tool(part)) = self.message.parts.get_mut(index) {
                    part.name.clone_from(&name);
                    part.input.clone_from(&input);
                    part.state = ToolState::Running;
                }
                if !self.pending_tools.iter().any(|call| call.call_id == id) {
                    self.pending_tools.push(PendingToolCall {
                        call_id: id,
                        name,
                        input,
                    });
                }
            }
            ModelEvent::StepFinished { reason, usage } => {
                self.finish_open_parts(emit);
                self.message.finish = Some(reason);
                self.message.usage = usage;
            }
        }
    }

    pub fn pending_tools(&self) -> &[PendingToolCall] {
        &self.pending_tools
    }

    pub fn complete_tool(&mut self, call_id: &str, output: ToolOutput) {
        let Some(index) = self.tools.get(call_id).copied() else {
            return;
        };
        if let Some(AssistantPart::Tool(part)) = self.message.parts.get_mut(index) {
            part.state = ToolState::Completed {
                title: output.title,
                output: output.output,
                diffs: output.diffs,
            };
        }
    }

    pub fn fail_tool(&mut self, call_id: &str, message: impl Into<String>) {
        let Some(index) = self.tools.get(call_id).copied() else {
            return;
        };
        if let Some(AssistantPart::Tool(part)) = self.message.parts.get_mut(index) {
            part.state = ToolState::Failed {
                message: message.into(),
            };
        }
    }

    pub fn fail_stream(&mut self, message: impl Into<String>, emit: &dyn Fn(HarnessEvent)) {
        self.finish_open_parts(emit);
        let message = message.into();
        self.message.error = Some(message.clone());
        for part in &mut self.message.parts {
            if let AssistantPart::Tool(tool) = part
                && matches!(tool.state, ToolState::Pending | ToolState::Running)
            {
                tool.state = ToolState::Failed {
                    message: "Tool execution aborted".to_string(),
                };
            }
        }
    }

    pub fn finish(mut self, emit: &dyn Fn(HarnessEvent)) -> (AssistantMessage, ProcessOutcome) {
        self.finish_open_parts(emit);
        let outcome = if self.message.error.is_some() {
            ProcessOutcome::Stop
        } else if self.message.has_tool_calls()
            || matches!(
                self.message.finish,
                Some(StopReason::ToolCalls | StopReason::Unknown) | None
            )
        {
            ProcessOutcome::Continue
        } else {
            ProcessOutcome::Stop
        };
        (self.message, outcome)
    }

    fn ensure_tool(&mut self, id: &str, name: &str) -> usize {
        if let Some(index) = self.tools.get(id) {
            return *index;
        }
        let index = self.message.parts.len();
        self.message.parts.push(AssistantPart::Tool(ToolPart {
            call_id: id.to_string(),
            name: name.to_string(),
            input: String::new(),
            state: ToolState::Pending,
        }));
        self.tools.insert(id.to_string(), index);
        index
    }

    fn finish_reasoning(&mut self, id: &str, emit: &dyn Fn(HarnessEvent)) {
        let Some(index) = self.reasoning.remove(id) else {
            return;
        };
        if let Some(AssistantPart::Reasoning(part)) = self.message.parts.get_mut(index) {
            part.completed = true;
        }
        emit(HarnessEvent::ReasoningFinished {
            run_id: self.run_id,
            reasoning_id: id.to_string(),
        });
    }

    fn finish_text(&mut self, id: &str, emit: &dyn Fn(HarnessEvent)) {
        let Some(index) = self.text.remove(id) else {
            return;
        };
        if let Some(AssistantPart::Text(part)) = self.message.parts.get_mut(index) {
            part.completed = true;
        }
        emit(HarnessEvent::TextFinished {
            run_id: self.run_id,
            text_id: id.to_string(),
        });
    }

    fn finish_open_parts(&mut self, emit: &dyn Fn(HarnessEvent)) {
        let reasoning: Vec<String> = self.reasoning.keys().cloned().collect();
        for id in reasoning {
            self.finish_reasoning(&id, emit);
        }
        let text: Vec<String> = self.text.keys().cloned().collect();
        for id in text {
            self.finish_text(&id, emit);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::harness::model::Usage;

    #[test]
    fn reasoning_is_streamed_then_closed_before_step_completion() {
        let message = AssistantMessage::new(2, 1);
        let mut processor = StreamProcessor::new(7, message);
        let events = Mutex::new(Vec::new());
        let emit = |event| events.lock().unwrap().push(event);

        processor.handle(ModelEvent::ReasoningStarted { id: "r1".into() }, &emit);
        processor.handle(
            ModelEvent::ReasoningDelta {
                id: "r1".into(),
                text: "Inspecting".into(),
            },
            &emit,
        );
        processor.handle(
            ModelEvent::StepFinished {
                reason: StopReason::Stop,
                usage: Usage::default(),
            },
            &emit,
        );
        let (message, outcome) = processor.finish(&emit);

        assert_eq!(outcome, ProcessOutcome::Stop);
        assert!(matches!(
            &message.parts[0],
            AssistantPart::Reasoning(ReasoningPart {
                text,
                completed: true,
                ..
            }) if text == "Inspecting"
        ));
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .any(|event| matches!(event, HarnessEvent::ReasoningFinished { .. }))
        );
    }
}
