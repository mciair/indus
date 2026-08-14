//! Indus agent harness.
//!
//! The harness owns orchestration only. Model selection and provider-specific
//! authentication remain outside this module and connect through `ModelTransport`.

mod builtin_tools;
mod classifier;
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
    time::Duration,
};

use classifier::GoalCategory;
use event::{HarnessEvent, PermissionReply, RunOutcome};
use jobs::{Job, JobService, now_ms};
use model::{
    CancellationToken, ModelContent, ModelMessage, ModelRequest, ModelTransport, Role,
    TransportError, TransportErrorKind, UnconfiguredTransport,
};
use permission::{PermissionAction, PermissionError, PermissionRule, PermissionService};
use persistence::SessionStore;
use processor::{ProcessOutcome, StreamProcessor};
use session::{AssistantMessage, AssistantPart, Session, TextPart};
use tool::{ToolContext, ToolRegistry};
use transport::ProviderTransport;

const DEFAULT_MAX_STEPS: usize = 64;
const DEFAULT_MAX_RETRIES: u16 = 3;
const DEFAULT_RETRY_DELAY_MS: u64 = 2_000;
const MAX_RETRY_DELAY_MS: u64 = 30_000;
const DOOM_LOOP_THRESHOLD: usize = 3;
const DEFAULT_COMPACTION_THRESHOLD_PERCENT: u8 = 85;

#[derive(Clone, Debug)]
pub struct HarnessConfig {
    pub system: Vec<String>,
    pub max_steps: usize,
    pub max_retries: u16,
    pub compaction_threshold_percent: u8,
    pub classify_prompts: bool,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            system: Vec::new(),
            max_steps: DEFAULT_MAX_STEPS,
            max_retries: DEFAULT_MAX_RETRIES,
            compaction_threshold_percent: DEFAULT_COMPACTION_THRESHOLD_PERCENT,
            classify_prompts: false,
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
        }
    }

    pub fn configured() -> Result<Self, TransportError> {
        let transport: Arc<dyn ModelTransport> = Arc::new(ProviderTransport::new()?);
        let jobs = JobService::load();
        let tools = builtin_tools::registry(jobs.clone());
        let permissions = PermissionService::new(default_permission_rules());
        let session_store = SessionStore::default_session();
        let session = session_store.load();
        let (event_tx, event_rx) = mpsc::channel();
        Ok(Self {
            transport,
            tools,
            permissions,
            config: HarnessConfig {
                system: vec![default_system_prompt()],
                classify_prompts: true,
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
        })
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
        let parent_id = {
            let mut session = self
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let id = session.push_user(prompt.clone());
            if let Some(store) = &self.session_store {
                let _ = store.save(&session);
            }
            id
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
            jobs: self.jobs.clone(),
            session_store: self.session_store.clone(),
        };
        let busy = Arc::clone(&self.busy);
        let active_cancellation = Arc::clone(&self.cancellation);
        thread::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| {
                runtime.run(run_id, parent_id, Some(&prompt))
            }));
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
            config: HarnessConfig {
                classify_prompts: false,
                ..self.config.clone()
            },
            session: Arc::clone(&session),
            events: self.event_tx.clone(),
            cancellation,
            jobs: self.jobs.clone(),
            session_store: None,
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
            let outcome = catch_unwind(AssertUnwindSafe(|| runtime.run(run_id, parent_id, None)))
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
    jobs: JobService,
    session_store: Option<SessionStore>,
}

impl Runtime {
    fn run(&self, run_id: u64, parent_id: u64, classify_goal: Option<&str>) -> RunOutcome {
        self.emit(HarnessEvent::RunStarted { run_id });

        if self.config.classify_prompts
            && let Some(goal) = classify_goal
        {
            self.emit(HarnessEvent::ClassifierStarted { run_id });
            let decision = classifier::classify(self.transport.as_ref(), goal, &self.cancellation)
                .unwrap_or_else(|_| {
                    classifier::Classification::fallback(
                        "Classifier failed; Indus will handle the request directly.",
                    )
                });
            self.emit(HarnessEvent::ClassifierFinished {
                run_id,
                category: format!("{:?}", decision.category),
                description: decision.short_description.clone(),
            });
            if decision.category == GoalCategory::TimeBasedJob {
                return self.schedule_job(run_id, parent_id, goal, decision);
            }
        }

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
                        return self.finish(run_id, RunOutcome::Cancelled);
                    }
                    TransportErrorKind::ContextOverflow => {
                        let (message, _) = processor.finish(&|event| self.emit(event));
                        self.push_assistant(message);
                        if self.compact(run_id) {
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
            if self.should_compact() && !self.compact(run_id) {
                self.emit(HarnessEvent::CompactionRequired { run_id });
                return self.finish(run_id, RunOutcome::CompactionRequired);
            }
            if outcome == ProcessOutcome::Stop {
                return self.finish(run_id, RunOutcome::Completed);
            }
        }

        self.emit(HarnessEvent::RunError {
            run_id,
            message: "The harness reached its configured step limit.".to_string(),
        });
        self.finish(run_id, RunOutcome::StepLimitReached)
    }

    fn schedule_job(
        &self,
        run_id: u64,
        parent_id: u64,
        goal: &str,
        decision: classifier::Classification,
    ) -> RunOutcome {
        let job = match self.jobs.create(goal, decision) {
            Ok(job) => job,
            Err(error) => {
                self.emit(HarnessEvent::RunError {
                    run_id,
                    message: format!("Could not persist the Job: {error}"),
                });
                return self.finish(run_id, RunOutcome::Failed);
            }
        };
        let schedule = job.schedule_description();
        self.emit(HarnessEvent::JobScheduled {
            run_id,
            job_id: job.id.clone(),
            name: job.name.clone(),
            schedule: schedule.clone(),
        });
        let text = format!("Scheduled {} {}.", job.name, schedule);
        let text_id = format!("job-{}", job.id);
        self.emit(HarnessEvent::TextStarted {
            run_id,
            text_id: text_id.clone(),
        });
        self.emit(HarnessEvent::TextDelta {
            run_id,
            text_id: text_id.clone(),
            text: text.clone(),
        });
        self.emit(HarnessEvent::TextFinished {
            run_id,
            text_id: text_id.clone(),
        });
        let mut assistant = AssistantMessage::new(
            self.session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .next_assistant(parent_id)
                .id,
            parent_id,
        );
        assistant.parts.push(AssistantPart::Text(TextPart {
            id: text_id,
            text,
            completed: true,
        }));
        assistant.finish = Some(model::StopReason::Stop);
        self.push_assistant(assistant);
        self.finish(run_id, RunOutcome::Scheduled)
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

    fn compact(&self, run_id: u64) -> bool {
        let source = {
            let session = self
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if session.messages.len() < 4 {
                return false;
            }
            session.summary_source(90_000)
        };
        self.emit(HarnessEvent::CompactionStarted { run_id });
        let request = ModelRequest {
            system: vec![
                "Summarize this coding session for continuation after context compaction. Preserve the user's goals and constraints, decisions, files inspected or changed, commands and test outcomes, unresolved errors, active Jobs, and exact next steps. Do not include hidden reasoning. Return only the continuation summary."
                    .to_string(),
            ],
            messages: vec![ModelMessage {
                role: Role::User,
                content: vec![ModelContent::Text(source)],
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
        session.compact(summary, 4);
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

    fn emit(&self, event: HarnessEvent) {
        let _ = self.events.send(event);
    }

    fn finish(&self, run_id: u64, outcome: RunOutcome) -> RunOutcome {
        self.emit(HarnessEvent::RunFinished { run_id, outcome });
        outcome
    }
}

fn context_compaction_threshold(context_window: Option<u64>, percent: u8) -> Option<u64> {
    let context_window = context_window.filter(|window| *window > 0)?;
    if percent == 0 || percent > 100 {
        return None;
    }
    let threshold = (u128::from(context_window) * u128::from(percent)).div_ceil(100);
    Some(threshold.min(u128::from(u64::MAX)) as u64)
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

    #[test]
    fn compaction_threshold_is_exactly_eighty_five_percent() {
        assert_eq!(
            context_compaction_threshold(Some(200_000), 85),
            Some(170_000)
        );
        assert_eq!(context_compaction_threshold(None, 85), None);
        assert_eq!(context_compaction_threshold(Some(200_000), 0), None);
    }
}
