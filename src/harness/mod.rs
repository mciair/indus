//! Indus agent harness.
//!
//! The harness owns orchestration only. Model selection and provider-specific
//! authentication remain outside this module and connect through `ModelTransport`.

pub mod event;
pub mod model;
pub mod permission;
mod processor;
pub mod session;
pub mod tool;

use std::{
    error::Error,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

use event::{HarnessEvent, PermissionReply, RunOutcome};
use model::{
    CancellationToken, ModelRequest, ModelTransport, TransportError, TransportErrorKind,
    UnconfiguredTransport,
};
use permission::{PermissionError, PermissionService};
use processor::{ProcessOutcome, StreamProcessor};
use session::Session;
use tool::{ToolContext, ToolRegistry};

const DEFAULT_MAX_STEPS: usize = 64;
const DEFAULT_MAX_RETRIES: u16 = 3;
const DEFAULT_RETRY_DELAY_MS: u64 = 2_000;
const MAX_RETRY_DELAY_MS: u64 = 30_000;
const DOOM_LOOP_THRESHOLD: usize = 3;

#[derive(Clone, Debug)]
pub struct HarnessConfig {
    pub system: Vec<String>,
    pub max_steps: usize,
    pub max_retries: u16,
    pub compaction_token_limit: Option<u64>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            system: Vec::new(),
            max_steps: DEFAULT_MAX_STEPS,
            max_retries: DEFAULT_MAX_RETRIES,
            compaction_token_limit: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HarnessError {
    Busy,
    EmptyPrompt,
}

impl fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Busy => "The current Indus session is already running.",
            Self::EmptyPrompt => "A prompt cannot be empty.",
        })
    }
}

impl Error for HarnessError {}

pub struct Harness {
    transport: Arc<dyn ModelTransport>,
    tools: ToolRegistry,
    permissions: PermissionService,
    config: HarnessConfig,
    session: Arc<Mutex<Session>>,
    event_tx: Sender<HarnessEvent>,
    event_rx: Mutex<Receiver<HarnessEvent>>,
    busy: Arc<AtomicBool>,
    next_run_id: AtomicU64,
    cancellation: Arc<Mutex<Option<CancellationToken>>>,
}

impl Harness {
    pub fn new(
        transport: Arc<dyn ModelTransport>,
        tools: ToolRegistry,
        permissions: PermissionService,
        config: HarnessConfig,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        Self {
            transport,
            tools,
            permissions,
            config,
            session: Arc::new(Mutex::new(Session::default())),
            event_tx,
            event_rx: Mutex::new(event_rx),
            busy: Arc::new(AtomicBool::new(false)),
            next_run_id: AtomicU64::new(0),
            cancellation: Arc::new(Mutex::new(None)),
        }
    }

    pub fn provider_neutral() -> Self {
        Self::new(
            Arc::new(UnconfiguredTransport),
            ToolRegistry::default(),
            PermissionService::default(),
            HarnessConfig::default(),
        )
    }

    pub fn submit(&self, prompt: impl Into<String>) -> Result<u64, HarnessError> {
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            return Err(HarnessError::EmptyPrompt);
        }
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(HarnessError::Busy);
        }

        let run_id = self.next_run_id.fetch_add(1, Ordering::Relaxed) + 1;
        let parent_id = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_user(prompt);
        let cancellation = CancellationToken::default();
        *self
            .cancellation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cancellation.clone());

        let runtime = Runtime {
            transport: Arc::clone(&self.transport),
            tools: self.tools.clone(),
            permissions: self.permissions.clone(),
            config: self.config.clone(),
            session: Arc::clone(&self.session),
            events: self.event_tx.clone(),
            cancellation: cancellation.clone(),
        };
        let busy = Arc::clone(&self.busy);
        let active_cancellation = Arc::clone(&self.cancellation);
        thread::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| runtime.run(run_id, parent_id)));
            if result.is_err() {
                let _ = runtime.events.send(HarnessEvent::RunError {
                    run_id,
                    message: "The harness stopped after an internal runtime panic.".to_string(),
                });
                let _ = runtime.events.send(HarnessEvent::RunFinished {
                    run_id,
                    outcome: RunOutcome::Failed,
                });
            }
            busy.store(false, Ordering::Release);
            let mut active = active_cancellation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *active = None;
        });
        Ok(run_id)
    }

    pub fn cancel(&self) -> bool {
        let active = self
            .cancellation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(cancellation) = active.as_ref() else {
            return false;
        };
        cancellation.cancel();
        true
    }

    pub fn reply_permission(&self, request_id: u64, reply: PermissionReply) -> bool {
        self.permissions.reply(request_id, reply)
    }

    pub fn drain_events(&self) -> Vec<HarnessEvent> {
        let receiver = self
            .event_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        receiver.try_iter().collect()
    }

    pub fn session_snapshot(&self) -> Session {
        self.session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::provider_neutral()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct Runtime {
    transport: Arc<dyn ModelTransport>,
    tools: ToolRegistry,
    permissions: PermissionService,
    config: HarnessConfig,
    session: Arc<Mutex<Session>>,
    events: Sender<HarnessEvent>,
    cancellation: CancellationToken,
}

impl Runtime {
    fn run(&self, run_id: u64, parent_id: u64) {
        self.emit(HarnessEvent::RunStarted { run_id });

        for step in 1..=self.config.max_steps.max(1) {
            if self.cancellation.is_cancelled() {
                self.finish(run_id, RunOutcome::Cancelled);
                return;
            }

            let (assistant, messages) = {
                let mut session = self
                    .session
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                (session.next_assistant(parent_id), session.model_messages())
            };
            let request = ModelRequest {
                system: self.config.system.clone(),
                messages,
                tools: self.tools.definitions(),
                step,
            };
            let mut processor = StreamProcessor::new(run_id, assistant);

            let stream_result = self.stream_with_retry(run_id, request, &mut processor);
            if let Err(error) = stream_result {
                match error.kind {
                    TransportErrorKind::Cancelled => {
                        processor.fail_stream(error.message, &|event| self.emit(event));
                        let (message, _) = processor.finish(&|event| self.emit(event));
                        self.push_assistant(message);
                        self.finish(run_id, RunOutcome::Cancelled);
                    }
                    TransportErrorKind::ContextOverflow => {
                        let (message, _) = processor.finish(&|event| self.emit(event));
                        self.push_assistant(message);
                        self.emit(HarnessEvent::CompactionRequired { run_id });
                        self.finish(run_id, RunOutcome::CompactionRequired);
                    }
                    TransportErrorKind::Retryable | TransportErrorKind::Fatal => {
                        processor.fail_stream(&error.message, &|event| self.emit(event));
                        let (message, _) = processor.finish(&|event| self.emit(event));
                        self.push_assistant(message);
                        self.emit(HarnessEvent::RunError {
                            run_id,
                            message: error.message,
                        });
                        self.finish(run_id, RunOutcome::Failed);
                    }
                }
                return;
            }

            let calls = processor.pending_tools().to_vec();
            let mut blocked = false;
            for call in calls {
                if self.cancellation.is_cancelled() {
                    processor.fail_tool(&call.call_id, "Tool execution cancelled");
                    blocked = true;
                    break;
                }
                let repeated = self
                    .session
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .repeated_tool_call_count(&call.name, &call.input);
                if repeated + 1 >= DOOM_LOOP_THRESHOLD {
                    let result = self.permissions.authorize(
                        run_id,
                        "doom_loop",
                        std::slice::from_ref(&call.name),
                        "Continue a repeatedly identical tool call",
                        &self.cancellation,
                        &|event| self.emit(event),
                    );
                    if let Err(error) = result {
                        processor.fail_tool(&call.call_id, error.to_string());
                        blocked = true;
                        break;
                    }
                }
                if !self.execute_tool(run_id, &call, &mut processor) {
                    blocked = true;
                    break;
                }
            }

            let (message, outcome) = processor.finish(&|event| self.emit(event));
            let usage = message.usage;
            self.push_assistant(message);

            if self.cancellation.is_cancelled() {
                self.finish(run_id, RunOutcome::Cancelled);
                return;
            }
            if blocked {
                self.finish(run_id, RunOutcome::Failed);
                return;
            }
            if self
                .config
                .compaction_token_limit
                .is_some_and(|limit| usage.total() >= limit)
            {
                self.emit(HarnessEvent::CompactionRequired { run_id });
                self.finish(run_id, RunOutcome::CompactionRequired);
                return;
            }
            if outcome == ProcessOutcome::Stop {
                self.finish(run_id, RunOutcome::Completed);
                return;
            }
        }

        self.emit(HarnessEvent::RunError {
            run_id,
            message: "The harness reached its configured step limit.".to_string(),
        });
        self.finish(run_id, RunOutcome::StepLimitReached);
    }

    fn stream_with_retry(
        &self,
        run_id: u64,
        request: ModelRequest,
        processor: &mut StreamProcessor,
    ) -> Result<(), TransportError> {
        let mut attempt = 0u16;
        loop {
            self.cancellation.check()?;
            self.emit(HarnessEvent::WaitingForResponse { run_id });
            let result = self.transport.stream(
                request.clone(),
                &mut |event| {
                    self.cancellation.check()?;
                    processor.handle(event, &|event| self.emit(event));
                    Ok(())
                },
                &self.cancellation,
            );
            let Err(error) = result else {
                return Ok(());
            };
            if error.kind != TransportErrorKind::Retryable || attempt >= self.config.max_retries {
                return Err(error);
            }
            attempt = attempt.saturating_add(1);
            let delay_ms = error.retry_after_ms.unwrap_or_else(|| {
                DEFAULT_RETRY_DELAY_MS
                    .saturating_mul(2u64.saturating_pow(u32::from(attempt.saturating_sub(1))))
                    .min(MAX_RETRY_DELAY_MS)
            });
            self.emit(HarnessEvent::RetryScheduled {
                run_id,
                attempt,
                delay_ms,
                message: error.message,
            });
            self.wait_or_cancel(delay_ms)?;
        }
    }

    fn execute_tool(
        &self,
        run_id: u64,
        call: &processor::PendingToolCall,
        processor: &mut StreamProcessor,
    ) -> bool {
        let Some(tool) = self.tools.get(&call.name) else {
            let message = format!("Tool not found: {}", call.name);
            processor.fail_tool(&call.call_id, &message);
            self.emit(HarnessEvent::ToolFailed {
                run_id,
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                message,
            });
            return true;
        };
        let definition = tool.definition();
        let permission = tool.permission(&call.input);
        self.emit(HarnessEvent::ToolStarted {
            run_id,
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            description: definition.description,
            input: call.input.clone(),
        });

        if let Err(error) = self.permissions.authorize(
            run_id,
            &permission.permission,
            &permission.patterns,
            &permission.description,
            &self.cancellation,
            &|event| self.emit(event),
        ) {
            let message = error.to_string();
            processor.fail_tool(&call.call_id, &message);
            self.emit(HarnessEvent::ToolFailed {
                run_id,
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                message,
            });
            return !matches!(
                error,
                PermissionError::Rejected | PermissionError::Cancelled
            );
        }

        let events = self.events.clone();
        let call_id = call.call_id.clone();
        let context = ToolContext::new(
            run_id,
            call.call_id.clone(),
            self.cancellation.clone(),
            move |text| {
                let _ = events.send(HarnessEvent::ToolOutput {
                    run_id,
                    call_id: call_id.clone(),
                    text,
                });
            },
        );
        match tool.execute(&call.input, &context) {
            Ok(output) => {
                self.emit(HarnessEvent::ToolFinished {
                    run_id,
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    title: output.title.clone(),
                    output: output.output.clone(),
                    diffs: output.diffs.clone(),
                });
                processor.complete_tool(&call.call_id, output);
                true
            }
            Err(error) => {
                let message = error.to_string();
                processor.fail_tool(&call.call_id, &message);
                self.emit(HarnessEvent::ToolFailed {
                    run_id,
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    message,
                });
                true
            }
        }
    }

    fn wait_or_cancel(&self, milliseconds: u64) -> Result<(), TransportError> {
        let mut remaining = milliseconds;
        while remaining > 0 {
            self.cancellation.check()?;
            let slice = remaining.min(50);
            thread::sleep(Duration::from_millis(slice));
            remaining -= slice;
        }
        self.cancellation.check()
    }

    fn push_assistant(&self, message: session::AssistantMessage) {
        self.session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_assistant(message);
    }

    fn emit(&self, event: HarnessEvent) {
        let _ = self.events.send(event);
    }

    fn finish(&self, run_id: u64, outcome: RunOutcome) {
        self.emit(HarnessEvent::RunFinished { run_id, outcome });
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    };

    use super::*;
    use crate::harness::{
        model::{ModelEvent, StopReason, ToolDefinition, Usage},
        permission::{PermissionAction, PermissionRule},
        tool::{HarnessTool, ToolError, ToolOutput, ToolPermission},
    };

    struct TextTransport;

    impl ModelTransport for TextTransport {
        fn stream(
            &self,
            _request: ModelRequest,
            on_event: &mut dyn FnMut(ModelEvent) -> Result<(), TransportError>,
            cancellation: &CancellationToken,
        ) -> Result<(), TransportError> {
            cancellation.check()?;
            on_event(ModelEvent::TextStarted { id: "t1".into() })?;
            on_event(ModelEvent::TextDelta {
                id: "t1".into(),
                text: "Hello from Indus".into(),
            })?;
            on_event(ModelEvent::TextFinished { id: "t1".into() })?;
            on_event(ModelEvent::StepFinished {
                reason: StopReason::Stop,
                usage: Usage::default(),
            })
        }
    }

    #[derive(Default)]
    struct ToolTransport {
        calls: AtomicUsize,
    }

    impl ModelTransport for ToolTransport {
        fn stream(
            &self,
            request: ModelRequest,
            on_event: &mut dyn FnMut(ModelEvent) -> Result<(), TransportError>,
            _cancellation: &CancellationToken,
        ) -> Result<(), TransportError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                assert_eq!(request.step, 1);
                on_event(ModelEvent::ToolCall {
                    id: "call-1".into(),
                    name: "status".into(),
                    input: "repository".into(),
                })?;
                on_event(ModelEvent::StepFinished {
                    reason: StopReason::ToolCalls,
                    usage: Usage::default(),
                })
            } else {
                assert!(request.messages.iter().any(|message| {
                    message.content.iter().any(|content| {
                        matches!(
                            content,
                            model::ModelContent::ToolResult { output, .. } if output == "clean"
                        )
                    })
                }));
                on_event(ModelEvent::TextStarted { id: "t2".into() })?;
                on_event(ModelEvent::TextDelta {
                    id: "t2".into(),
                    text: "The repository is clean.".into(),
                })?;
                on_event(ModelEvent::TextFinished { id: "t2".into() })?;
                on_event(ModelEvent::StepFinished {
                    reason: StopReason::Stop,
                    usage: Usage::default(),
                })
            }
        }
    }

    struct StatusTool;

    impl HarnessTool for StatusTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "status".into(),
                description: "Check repository status".into(),
                input_schema: "{}".into(),
            }
        }

        fn permission(&self, input: &str) -> ToolPermission {
            ToolPermission {
                permission: "status".into(),
                patterns: vec![input.to_string()],
                description: "Check repository status".into(),
            }
        }

        fn execute(&self, _input: &str, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                title: "Checked repository status".into(),
                output: "clean".into(),
                diffs: Vec::new(),
            })
        }
    }

    #[test]
    fn harness_streams_text_and_completes_the_run() {
        let harness = Harness::new(
            Arc::new(TextTransport),
            ToolRegistry::default(),
            PermissionService::default(),
            HarnessConfig::default(),
        );
        harness.submit("hello").unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events.extend(harness.drain_events());
            if events
                .iter()
                .any(|event| matches!(event, HarnessEvent::RunFinished { .. }))
            {
                break;
            }
            thread::yield_now();
        }

        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::TextDelta { text, .. } if text == "Hello from Indus"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::RunFinished {
                outcome: RunOutcome::Completed,
                ..
            }
        )));
    }

    #[test]
    fn tool_results_continue_into_the_next_model_step() {
        let tools = ToolRegistry::default();
        tools.register(StatusTool);
        let permissions = PermissionService::new(vec![PermissionRule {
            permission: "status".into(),
            pattern: "*".into(),
            action: PermissionAction::Allow,
        }]);
        let harness = Harness::new(
            Arc::new(ToolTransport::default()),
            tools,
            permissions,
            HarnessConfig::default(),
        );
        harness.submit("inspect the repository").unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events.extend(harness.drain_events());
            if events
                .iter()
                .any(|event| matches!(event, HarnessEvent::RunFinished { .. }))
            {
                break;
            }
            thread::yield_now();
        }

        assert!(
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::ToolStarted { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::ToolFinished { .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::TextDelta { text, .. } if text == "The repository is clean."
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::RunFinished {
                outcome: RunOutcome::Completed,
                ..
            }
        )));
    }
}
