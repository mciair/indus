//! Provider-neutral model stream contract used by the Indus harness.

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelContent {
    Text(String),
    ToolCall {
        call_id: String,
        name: String,
        input: String,
    },
    ToolResult {
        call_id: String,
        name: String,
        output: String,
        is_error: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelMessage {
    pub role: Role,
    pub content: Vec<ModelContent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    pub system: Vec<String>,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolDefinition>,
    pub step: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl Usage {
    pub fn total(self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.reasoning_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    Stop,
    ToolCalls,
    Length,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelEvent {
    StepStarted,
    ReasoningStarted {
        id: String,
    },
    ReasoningDelta {
        id: String,
        text: String,
    },
    ReasoningFinished {
        id: String,
    },
    TextStarted {
        id: String,
    },
    TextDelta {
        id: String,
        text: String,
    },
    TextFinished {
        id: String,
    },
    ToolInputStarted {
        id: String,
        name: String,
    },
    ToolInputDelta {
        id: String,
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: String,
    },
    StepFinished {
        reason: StopReason,
        usage: Usage,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorKind {
    Retryable,
    ContextOverflow,
    Cancelled,
    Fatal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub message: String,
    pub retry_after_ms: Option<u64>,
}

impl TransportError {
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Fatal,
            message: message.into(),
            retry_after_ms: None,
        }
    }

    pub fn cancelled() -> Self {
        Self {
            kind: TransportErrorKind::Cancelled,
            message: "Model stream cancelled".to_string(),
            retry_after_ms: None,
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for TransportError {}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn check(&self) -> Result<(), TransportError> {
        if self.is_cancelled() {
            Err(TransportError::cancelled())
        } else {
            Ok(())
        }
    }
}

pub trait ModelTransport: Send + Sync + 'static {
    fn stream(
        &self,
        request: ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent) -> Result<(), TransportError>,
        cancellation: &CancellationToken,
    ) -> Result<(), TransportError>;
}

/// Safe placeholder used until Indus receives its model transport decision.
#[derive(Debug, Default)]
pub struct UnconfiguredTransport;

impl ModelTransport for UnconfiguredTransport {
    fn stream(
        &self,
        _request: ModelRequest,
        _on_event: &mut dyn FnMut(ModelEvent) -> Result<(), TransportError>,
        cancellation: &CancellationToken,
    ) -> Result<(), TransportError> {
        cancellation.check()?;
        Err(TransportError::fatal(
            "The Indus model transport has not been configured yet.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_shared_between_transport_handles() {
        let first = CancellationToken::default();
        let second = first.clone();
        first.cancel();
        assert!(second.is_cancelled());
        assert_eq!(
            second.check().unwrap_err().kind,
            TransportErrorKind::Cancelled
        );
    }

    #[test]
    fn usage_total_saturates_instead_of_overflowing() {
        let usage = Usage {
            input_tokens: u64::MAX,
            output_tokens: 1,
            ..Usage::default()
        };
        assert_eq!(usage.total(), u64::MAX);
    }
}
