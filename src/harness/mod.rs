//! Indus agent harness.
//!
//! The harness owns orchestration only. Model selection and provider-specific
//! authentication remain outside this module and connect through `ModelTransport`.

mod builtin_tools;
pub mod event;
pub mod jobs;
pub mod model;
pub mod permission;
mod persistence;
mod processor;
pub mod session;
pub mod tool;
mod transport;

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
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use event::{HarnessEvent, PermissionReply, RunOutcome};
use jobs::{Job, JobService, now_ms};
use model::{
    CancellationToken, ModelContent, ModelMessage, ModelRequest, ModelTransport, Role,
    TransportError, TransportErrorKind, UnconfiguredTransport,
};
use permission::{PermissionAction, PermissionError, PermissionRule, PermissionService};
use persistence::SessionStore;
pub use persistence::SessionSummary;
use processor::{ProcessOutcome, StreamProcessor};
use session::{AssistantPart, CompactionInput, Session, title_from_first_prompt};
use tool::{ToolContext, ToolRegistry};
use transport::ProviderTransport;

const DEFAULT_MAX_STEPS: usize = 64;
const DEFAULT_MAX_RETRIES: u16 = 3;
const DEFAULT_RETRY_DELAY_MS: u64 = 2_000;
const MAX_RETRY_DELAY_MS: u64 = 30_000;
const DOOM_LOOP_THRESHOLD: usize = 3;
const DEFAULT_COMPACTION_THRESHOLD_PERCENT: u8 = 85;
const COMPACTION_SOURCE_LIMIT: usize = 90_000;
const COMPACTION_PRESERVED_USER_TURNS: usize = 2;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SessionMode {
    #[default]
    Normal,
    Plan,
    AlwaysApprove,
}

impl SessionMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Plan => "Plan",
            Self::AlwaysApprove => "Always-Approve",
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Normal => Self::Plan,
            Self::Plan => Self::AlwaysApprove,
            Self::AlwaysApprove => Self::Normal,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HarnessConfig {
    pub system: Vec<String>,
    pub max_steps: usize,
    pub max_retries: u16,
    pub compaction_threshold_percent: u8,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            system: Vec::new(),
            max_steps: DEFAULT_MAX_STEPS,
            max_retries: DEFAULT_MAX_RETRIES,
            compaction_threshold_percent: DEFAULT_COMPACTION_THRESHOLD_PERCENT,
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
    jobs: JobService,
    session_store: Option<SessionStore>,
    mode: Arc<Mutex<SessionMode>>,
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
            jobs: JobService::load(),
            session_store: None,
            mode: Arc::new(Mutex::new(SessionMode::Normal)),
        }
    }

    pub fn configured() -> Result<Self, TransportError> {
        Self::configured_with_session(None)
    }

    pub fn configured_with_session(session_id: Option<&str>) -> Result<Self, TransportError> {
        let transport: Arc<dyn ModelTransport> = Arc::new(ProviderTransport::new()?);
        let jobs = JobService::load();
        let tools = builtin_tools::registry(jobs.clone());
        let permissions = PermissionService::new(default_permission_rules());
        let session_store = SessionStore::database().map_err(|error| {
            TransportError::fatal(format!("Could not initialize session history: {error:#}"))
        })?;
        let directory = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let session = match session_id {
            Some(id) => session_store
                .load(id)
                .map_err(|error| {
                    TransportError::fatal(format!("Could not resume {id}: {error:#}"))
                })?
                .ok_or_else(|| TransportError::fatal(format!("Session not found: {id}")))?,
            None => Session::unallocated(directory),
        };
        let (event_tx, event_rx) = mpsc::channel();
        Ok(Self {
            transport,
            tools,
            permissions,
            config: HarnessConfig {
                system: vec![default_system_prompt()],
                ..HarnessConfig::default()
            },
            session: Arc::new(Mutex::new(session)),
            event_tx,
            event_rx: Mutex::new(event_rx),
            busy: Arc::new(AtomicBool::new(false)),
            next_run_id: AtomicU64::new(0),
            cancellation: Arc::new(Mutex::new(None)),
            jobs,
            session_store: Some(session_store),
            mode: Arc::new(Mutex::new(SessionMode::Normal)),
        })
    }

    pub fn list_sessions(&self, query: Option<&str>) -> anyhow::Result<Vec<SessionSummary>> {
        self.session_store
            .as_ref()
            .map_or_else(|| Ok(Vec::new()), |store| store.list(query))
    }

    pub fn new_session(&self) -> anyhow::Result<Session> {
        if self.is_busy() {
            return Err(anyhow::anyhow!(HarnessError::Busy));
        }
        let directory = std::env::current_dir()?.to_string_lossy().into_owned();
        let session = Session::unallocated(directory);
        *self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = session.clone();
        Ok(session)
    }

    pub fn resume_session(&self, session_id: &str) -> anyhow::Result<Session> {
        if self.is_busy() {
            return Err(anyhow::anyhow!(HarnessError::Busy));
        }
        if !session_id.starts_with("ses-i_") {
            return Err(anyhow::anyhow!("Invalid Indus session ID: {session_id}"));
        }
        let store = self
            .session_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Session history is unavailable"))?;
        let session = store
            .load(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;
        *self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = session.clone();
        Ok(session)
    }

    pub fn rename_session(&self, title: &str) -> anyhow::Result<String> {
        if self.is_busy() {
            return Err(anyhow::anyhow!(HarnessError::Busy));
        }
        let title = title.trim();
        if title.is_empty() {
            return Err(anyhow::anyhow!("Session title cannot be empty"));
        }
        if title.chars().count() > 100 {
            return Err(anyhow::anyhow!(
                "Session title must be 100 characters or fewer"
            ));
        }
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !session.rename(title) {
            return Err(anyhow::anyhow!(
                "This conversation has no saved session to rename yet"
            ));
        }
        if let Some(store) = &self.session_store {
            store.save(&session)?;
        }
        Ok(title.to_string())
    }

    pub fn session_info(&self) -> String {
        let session = self.session_snapshot();
        let id = if session.is_allocated() {
            session.id.as_str()
        } else {
            "Not allocated yet"
        };
        let title = session.title.as_deref().unwrap_or("Untitled");
        let provider = session.provider_id.as_deref().unwrap_or("Not recorded");
        let model = session.model_id.as_deref().unwrap_or("Not recorded");
        let context = session.current_context_tokens();
        let window = self
            .transport
            .context_window()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "Unknown".to_string());
        [
            "| Session | Value |".to_string(),
            "| --- | --- |".to_string(),
            format!("| ID | {id} |"),
            format!("| Title | {title} |"),
            format!("| Mode | {} |", self.mode().label()),
            format!("| Provider | {provider} |"),
            format!("| Model | {model} |"),
            format!("| Messages | {} |", session.messages.len()),
            format!("| Context | {context} / {window} tokens |"),
            format!("| Directory | {} |", session.directory),
        ]
        .join("\n")
    }

    pub fn delete_session(&self) -> anyhow::Result<(Session, String)> {
        if self.is_busy() {
            return Err(anyhow::anyhow!(HarnessError::Busy));
        }
        let current = self.session_snapshot();
        if !current.is_allocated() {
            return Err(anyhow::anyhow!(
                "This conversation has no saved session to delete"
            ));
        }
        let store = self
            .session_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Session history is unavailable"))?;
        if !store.delete(&current.id)? {
            return Err(anyhow::anyhow!("Session not found: {}", current.id));
        }
        let session = Session::unallocated(current.directory);
        *self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = session.clone();
        Ok((session, current.id))
    }

    pub fn set_mode(&self, mode: SessionMode) -> anyhow::Result<()> {
        if self.is_busy() {
            return Err(anyhow::anyhow!(HarnessError::Busy));
        }
        self.permissions
            .set_always_approve(mode == SessionMode::AlwaysApprove);
        *self
            .mode
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = mode;
        Ok(())
    }

    pub fn mode(&self) -> SessionMode {
        *self
            .mode
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn edit_previous_prompt(&self) -> anyhow::Result<(Session, String)> {
        if self.is_busy() {
            return Err(anyhow::anyhow!(HarnessError::Busy));
        }
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prompt = session
            .rewind_last_turn()
            .ok_or_else(|| anyhow::anyhow!("There is no previous prompt to edit"))?;
        if let Some(store) = &self.session_store {
            store.save(&session)?;
        }
        Ok((session.clone(), prompt))
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
        let selection = crate::provider::ProviderStore::load()
            .active_selection()
            .cloned();
        let provider_id = selection
            .as_ref()
            .map(|selection| format!("{:?}", selection.provider).to_lowercase());
        let model_id = selection.map(|selection| selection.model_id);
        let (parent_id, identity) = {
            let mut session = self
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let id = session.push_user(prompt.clone());
            let identity = if session.is_allocated() || self.session_store.is_none() {
                None
            } else {
                title_from_first_prompt(&prompt).map(|title| SessionIdentity {
                    title,
                    provider_id,
                    model_id,
                })
            };
            if let Some(store) = &self.session_store {
                let _ = store.save(&session);
            }
            (id, identity)
        };
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
            session_store: self.session_store.clone(),
            mode: self.mode(),
            identity,
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

    pub fn compact_context(&self, instructions: Option<String>) -> anyhow::Result<u64> {
        {
            let session = self
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if session
                .compaction_input(COMPACTION_PRESERVED_USER_TURNS, COMPACTION_SOURCE_LIMIT)
                .is_none()
            {
                return Err(anyhow::anyhow!(
                    "There is no conversation context to compact"
                ));
            }
        }
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(anyhow::anyhow!(HarnessError::Busy));
        }

        let run_id = self.next_run_id.fetch_add(1, Ordering::Relaxed) + 1;
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
            cancellation,
            session_store: self.session_store.clone(),
            mode: self.mode(),
            identity: None,
        };
        let busy = Arc::clone(&self.busy);
        let active_cancellation = Arc::clone(&self.cancellation);
        thread::spawn(move || {
            runtime.emit(HarnessEvent::RunStarted { run_id });
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                if runtime.compact(run_id, instructions.as_deref()) {
                    RunOutcome::Compacted
                } else {
                    RunOutcome::CompactionRequired
                }
            }))
            .unwrap_or(RunOutcome::Failed);
            runtime.finish(run_id, outcome);
            busy.store(false, Ordering::Release);
            *active_cancellation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
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

    pub fn poll_jobs(&self) {
        if self.busy.load(Ordering::Acquire) {
            return;
        }
        let Some(job) = self.jobs.due(now_ms()).into_iter().next() else {
            return;
        };
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let started_at = now_ms();
        if self
            .jobs
            .mark_started(&job.id, started_at)
            .ok()
            .flatten()
            .is_none()
        {
            self.busy.store(false, Ordering::Release);
            return;
        }

        let run_id = self.next_run_id.fetch_add(1, Ordering::Relaxed) + 1;
        let session = Arc::new(Mutex::new(Session::new(format!("job:{}", job.id))));
        let prompt = persistent_job_prompt(&job);
        let parent_id = session
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
            session: Arc::clone(&session),
            events: self.event_tx.clone(),
            cancellation,
            session_store: None,
            mode: self.mode(),
            identity: None,
        };
        let events = self.event_tx.clone();
        let busy = Arc::clone(&self.busy);
        let active_cancellation = Arc::clone(&self.cancellation);
        let jobs = self.jobs.clone();
        thread::spawn(move || {
            let _ = events.send(HarnessEvent::JobRunStarted {
                run_id,
                job_id: job.id.clone(),
                name: job.name.clone(),
            });
            let outcome = catch_unwind(AssertUnwindSafe(|| runtime.run(run_id, parent_id)))
                .unwrap_or(RunOutcome::Failed);
            let succeeded = outcome == RunOutcome::Completed;
            let result = session_result(&session);
            if succeeded {
                let _ = jobs.mark_completed(&job.id, now_ms(), result);
            } else {
                let _ = jobs.mark_failed(&job.id, now_ms(), result);
            }
            let _ = events.send(HarnessEvent::JobRunFinished {
                run_id,
                job_id: job.id,
                name: job.name,
                succeeded,
            });
            busy.store(false, Ordering::Release);
            *active_cancellation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        });
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
    session_store: Option<SessionStore>,
    mode: SessionMode,
    identity: Option<SessionIdentity>,
}

#[derive(Clone, Debug)]
struct SessionIdentity {
    title: String,
    provider_id: Option<String>,
    model_id: Option<String>,
}

impl Runtime {
    fn run(&self, run_id: u64, parent_id: u64) -> RunOutcome {
        self.emit(HarnessEvent::RunStarted { run_id });

        'steps: for step in 1..=self.config.max_steps.max(1) {
            if self.cancellation.is_cancelled() {
                return self.finish(run_id, RunOutcome::Cancelled);
            }

            let (assistant, messages) = {
                let mut session = self
                    .session
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                (session.next_assistant(parent_id), session.model_messages())
            };
            let mut system = self.config.system.clone();
            if self.mode == SessionMode::Plan {
                system.push(plan_mode_prompt());
            }
            let tools = self
                .tools
                .definitions()
                .into_iter()
                .filter(|tool| self.mode != SessionMode::Plan || plan_tool_allowed(&tool.name))
                .collect();
            let request = ModelRequest {
                system,
                messages,
                tools,
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
                        return self.finish(run_id, RunOutcome::Cancelled);
                    }
                    TransportErrorKind::ContextOverflow => {
                        let (message, _) = processor.finish(&|event| self.emit(event));
                        self.push_assistant(message);
                        if self.compact(run_id, None) {
                            continue 'steps;
                        }
                        self.emit(HarnessEvent::CompactionRequired { run_id });
                        return self.finish(run_id, RunOutcome::CompactionRequired);
                    }
                    TransportErrorKind::Retryable | TransportErrorKind::Fatal => {
                        processor.fail_stream(&error.message, &|event| self.emit(event));
                        let (message, _) = processor.finish(&|event| self.emit(event));
                        self.push_assistant(message);
                        self.emit(HarnessEvent::RunError {
                            run_id,
                            message: error.message,
                        });
                        return self.finish(run_id, RunOutcome::Failed);
                    }
                }
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
            self.push_assistant(message);

            if self.cancellation.is_cancelled() {
                return self.finish(run_id, RunOutcome::Cancelled);
            }
            if blocked {
                return self.finish(run_id, RunOutcome::Failed);
            }
            if self.should_compact() && !self.compact(run_id, None) {
                self.emit(HarnessEvent::CompactionRequired { run_id });
                return self.finish(run_id, RunOutcome::CompactionRequired);
            }
            if outcome == ProcessOutcome::Stop {
                self.ensure_session_identity(run_id);
                return self.finish(run_id, RunOutcome::Completed);
            }
        }

        self.emit(HarnessEvent::RunError {
            run_id,
            message: "The harness reached its configured step limit.".to_string(),
        });
        self.finish(run_id, RunOutcome::StepLimitReached)
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
        if self.mode == SessionMode::Plan && !plan_tool_allowed(&call.name) {
            let message = format!("{} is unavailable in Plan mode", call.name);
            processor.fail_tool(&call.call_id, &message);
            self.emit(HarnessEvent::ToolFailed {
                run_id,
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                message,
            });
            return true;
        }
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

    fn compact(&self, run_id: u64, instructions: Option<&str>) -> bool {
        let input = {
            let session = self
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(input) =
                session.compaction_input(COMPACTION_PRESERVED_USER_TURNS, COMPACTION_SOURCE_LIMIT)
            else {
                return false;
            };
            input
        };
        self.emit(HarnessEvent::CompactionStarted { run_id });
        let request = ModelRequest {
            system: vec![compaction_system_prompt()],
            messages: vec![ModelMessage {
                role: Role::User,
                content: vec![ModelContent::Text(compaction_user_prompt(
                    &input,
                    instructions,
                ))],
            }],
            tools: Vec::new(),
            step: 0,
        };
        let mut summary = String::new();
        let result = self.transport.stream(
            request,
            &mut |event| {
                if let model::ModelEvent::TextDelta { text, .. } = event {
                    summary.push_str(&text);
                }
                Ok(())
            },
            &self.cancellation,
        );
        if result.is_err() || summary.trim().is_empty() {
            return false;
        }
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        session.compact_at(summary, input.preserve_from);
        if let Some(store) = &self.session_store {
            let _ = store.save(&session);
        }
        drop(session);
        self.emit(HarnessEvent::CompactionFinished { run_id });
        true
    }

    fn should_compact(&self) -> bool {
        let Some(threshold) = context_compaction_threshold(
            self.transport.context_window(),
            self.config.compaction_threshold_percent,
        ) else {
            return false;
        };
        let context_tokens = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .current_context_tokens();
        context_tokens > 0 && context_tokens >= threshold
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
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        session.push_assistant(message);
        if let Some(store) = &self.session_store {
            let _ = store.save(&session);
        }
    }

    fn ensure_session_identity(&self, run_id: u64) {
        let Some(identity) = self.identity.as_ref() else {
            return;
        };
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session_id = generate_session_id();
        if !session.allocate(
            session_id.clone(),
            identity.title.clone(),
            identity.provider_id.clone(),
            identity.model_id.clone(),
        ) {
            return;
        }
        if let Some(store) = &self.session_store
            && store.save(&session).is_err()
        {
            return;
        }
        self.emit(HarnessEvent::SessionCreated {
            run_id,
            session_id,
            title: identity.title.clone(),
        });
    }

    fn emit(&self, event: HarnessEvent) {
        let _ = self.events.send(event);
    }

    fn finish(&self, run_id: u64, outcome: RunOutcome) -> RunOutcome {
        self.emit(HarnessEvent::RunFinished { run_id, outcome });
        outcome
    }
}

fn generate_session_id() -> String {
    static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    format!("ses-i_{timestamp:x}{:x}{sequence:x}", std::process::id())
}

fn context_compaction_threshold(context_window: Option<u64>, percent: u8) -> Option<u64> {
    let context_window = context_window.filter(|window| *window > 0)?;
    if percent == 0 || percent > 100 {
        return None;
    }
    let threshold = (u128::from(context_window) * u128::from(percent)).div_ceil(100);
    Some(threshold.min(u128::from(u64::MAX)) as u64)
}

fn plan_mode_prompt() -> String {
    [
        "PLAN MODE is active.",
        "Inspect the project using read-only tools and produce a concrete implementation plan.",
        "Do not edit files, run mutating commands, clone repositories, or create or modify Jobs.",
        "Ask only questions that materially block a reliable plan, and do not claim changes were made.",
    ]
    .join("\n")
}

fn plan_tool_allowed(name: &str) -> bool {
    matches!(
        name,
        "read" | "glob" | "grep" | "web_fetch" | "web_search" | "repo_overview"
    )
}

fn compaction_system_prompt() -> String {
    [
        "You are an anchored context summarization assistant for coding sessions.",
        "Summarize only the conversation history you are given. The newest turns are kept verbatim outside your summary, so focus on the older context that still matters for continuing the work.",
        "If the prompt includes a <previous-summary> block, treat it as the current anchored summary. Update it with the new history by preserving still-true details, removing stale details, and merging in new facts.",
        "Always follow the exact output structure requested by the user prompt. Keep every section, preserve exact file paths and identifiers when known, and prefer terse bullets over paragraphs.",
        "Do not answer the conversation itself. Do not mention that you are summarizing, compacting, or merging context. Do not include hidden reasoning. Respond in the same language as the conversation.",
    ]
    .join("\n")
}

fn compaction_user_prompt(input: &CompactionInput, instructions: Option<&str>) -> String {
    let mut prompt = String::new();
    if let Some(previous) = input.previous_summary.as_deref() {
        prompt.push_str("<previous-summary>\n");
        prompt.push_str(previous);
        prompt.push_str("\n</previous-summary>\n\n");
    }
    prompt.push_str("<conversation-history>\n");
    prompt.push_str(&input.history);
    prompt.push_str("</conversation-history>\n\n");
    if let Some(instructions) = instructions
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        prompt.push_str("Additional user guidance:\n");
        prompt.push_str(instructions);
        prompt.push_str("\n\n");
    }
    prompt.push_str(
        "Return exactly this structure. Keep every section and use terse bullets.\n\n\
## Goal\n\
- [single-sentence task summary]\n\n\
## Constraints & Preferences\n\
- [...]\n\n\
## Progress\n\
### Done\n\
- [...]\n\
### In Progress\n\
- [...]\n\
### Blocked\n\
- [...]\n\n\
## Key Decisions\n\
- [...]\n\n\
## Next Steps\n\
- [...]\n\n\
## Critical Context\n\
- [...]\n\n\
## Relevant Files\n\
- [...]",
    );
    prompt
}

fn default_permission_rules() -> Vec<PermissionRule> {
    [
        "read",
        "glob",
        "grep",
        "todo",
        "job",
        "repo_overview",
        "web_search",
        "web_fetch",
    ]
    .into_iter()
    .map(|permission| PermissionRule {
        permission: permission.to_string(),
        pattern: "*".to_string(),
        action: PermissionAction::Allow,
    })
    .chain([
        PermissionRule {
            permission: "edit".into(),
            pattern: "*".into(),
            action: PermissionAction::Ask,
        },
        PermissionRule {
            permission: "shell".into(),
            pattern: "*".into(),
            action: PermissionAction::Ask,
        },
        PermissionRule {
            permission: "repo_clone".into(),
            pattern: "*".into(),
            action: PermissionAction::Ask,
        },
        PermissionRule {
            permission: "doom_loop".into(),
            pattern: "*".into(),
            action: PermissionAction::Ask,
        },
    ])
    .collect()
}

fn default_system_prompt() -> String {
    [
        "You are Indus, an AI coding agent operating in the user's current working directory.",
        "Inspect relevant files before changing them. Use the provided tools to complete work, not merely describe it.",
        "Keep changes focused, preserve unrelated user work, and verify material edits with appropriate tests or checks.",
        "Use read, glob, and grep for discovery; edit, write, or apply_patch for precise changes; and shell for builds, tests, and Git operations.",
        "Use web_search for current external information and web_fetch for a known page. Cite useful source URLs in the response.",
        "Do not expose hidden reasoning. Stream only concise progress reasoning suitable for the user interface.",
        "Never claim a command, edit, test, or scheduled Job succeeded unless its tool result confirms it.",
    ]
    .join("\n")
}

fn persistent_job_prompt(job: &Job) -> String {
    [
        "# Persistent Job Brief".to_string(),
        String::new(),
        format!("Job: {}", job.name),
        format!("ID: {}", job.id),
        format!("Schedule: {}", job.schedule_description()),
        String::new(),
        "## Original Goal".to_string(),
        job.goal.clone(),
        String::new(),
        "## Execution Contract".to_string(),
        "Execute one useful scheduled run now. Inspect current state, perform the requested work, verify the result, and return a concise run summary.".to_string(),
        "Do not assume access to the original conversation beyond this brief. If a required decision is missing, state the blocker and the exact question.".to_string(),
        "Persist outputs in the repository or destination named by the goal.".to_string(),
    ]
    .join("\n")
}

fn session_result(session: &Arc<Mutex<Session>>) -> String {
    let session = session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    session
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            session::SessionMessage::Assistant(message) => {
                let text = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        AssistantPart::Text(part) => Some(part.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        })
        .unwrap_or_else(|| "The scheduled run produced no text result.".to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

    #[test]
    fn compaction_threshold_is_exactly_eighty_five_percent() {
        assert_eq!(
            context_compaction_threshold(Some(200_000), 85),
            Some(170_000)
        );
        assert_eq!(context_compaction_threshold(None, 85), None);
        assert_eq!(context_compaction_threshold(Some(200_000), 0), None);
    }

    #[test]
    fn plan_mode_only_exposes_read_only_discovery_tools() {
        for tool in [
            "read",
            "glob",
            "grep",
            "web_fetch",
            "web_search",
            "repo_overview",
        ] {
            assert!(plan_tool_allowed(tool));
        }
        for tool in ["edit", "write", "apply_patch", "shell", "job", "repo_clone"] {
            assert!(!plan_tool_allowed(tool));
        }
    }

    #[test]
    fn compaction_prompt_preserves_the_mirror_continuation_structure() {
        let input = CompactionInput {
            previous_summary: Some("## Goal\n- Existing goal".into()),
            history: "User:\nContinue the work\n".into(),
            preserve_from: 2,
        };
        let prompt = compaction_user_prompt(&input, Some("retain exact paths"));

        assert!(prompt.contains("<previous-summary>"));
        assert!(prompt.contains("retain exact paths"));
        for heading in [
            "## Goal",
            "## Constraints & Preferences",
            "## Progress",
            "## Key Decisions",
            "## Next Steps",
            "## Critical Context",
            "## Relevant Files",
        ] {
            assert!(prompt.contains(heading));
        }
    }

    #[test]
    fn manual_compaction_uses_the_model_and_replaces_old_context() {
        let harness = Harness::new(
            Arc::new(TextTransport),
            ToolRegistry::default(),
            PermissionService::default(),
            HarnessConfig::default(),
        );
        harness
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_user("large conversation context");
        harness.compact_context(None).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events.extend(harness.drain_events());
            if events.iter().any(|event| {
                matches!(
                    event,
                    HarnessEvent::RunFinished {
                        outcome: RunOutcome::Compacted,
                        ..
                    }
                )
            }) {
                break;
            }
            thread::yield_now();
        }

        assert!(
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::CompactionFinished { .. }))
        );
        assert!(matches!(
            &harness.session_snapshot().messages[0],
            session::SessionMessage::User(message)
                if message.text.contains("Hello from Indus")
        ));
    }

    #[test]
    fn first_completed_chat_uses_the_initial_prompt_as_its_identity() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("indus-harness-session-{unique}"));
        let store = SessionStore::at(root.join("indus.db")).unwrap();
        let mut harness = Harness::new(
            Arc::new(TextTransport),
            ToolRegistry::default(),
            PermissionService::default(),
            HarnessConfig::default(),
        );
        harness.session_store = Some(store.clone());
        harness.submit("inspect this repository").unwrap();

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

        let session = harness.session_snapshot();
        assert!(session.id.starts_with("ses-i_"));
        assert_eq!(session.title.as_deref(), Some("inspect this repository"));
        assert!(matches!(
            store.load(&session.id).unwrap(),
            Some(stored) if stored == session
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::SessionCreated { session_id, .. } if session_id == &session.id
        )));
        drop(harness);
        let _ = fs::remove_dir_all(root);
    }
}
