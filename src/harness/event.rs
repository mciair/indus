//! Events emitted by the Indus harness for presentation and external clients.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    Completed,
    Compacted,
    Scheduled,
    Cancelled,
    Failed,
    CompactionRequired,
    StepLimitReached,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionReply {
    AllowOnce,
    AllowAlways,
    Reject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DiffKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiffLine {
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub kind: DiffKind,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HarnessEvent {
    SessionCreated {
        run_id: u64,
        session_id: String,
        title: String,
    },
    SessionTitleUpdated {
        run_id: u64,
        session_id: String,
        title: String,
    },
    RunStarted {
        run_id: u64,
    },
    JobScheduled {
        run_id: u64,
        job_id: String,
        name: String,
        schedule: String,
    },
    JobRunStarted {
        run_id: u64,
        job_id: String,
        name: String,
    },
    JobRunFinished {
        run_id: u64,
        job_id: String,
        name: String,
        succeeded: bool,
    },
    WaitingForResponse {
        run_id: u64,
    },
    ReasoningStarted {
        run_id: u64,
        reasoning_id: String,
    },
    ReasoningDelta {
        run_id: u64,
        reasoning_id: String,
        text: String,
    },
    ReasoningFinished {
        run_id: u64,
        reasoning_id: String,
    },
    TextStarted {
        run_id: u64,
        text_id: String,
    },
    TextDelta {
        run_id: u64,
        text_id: String,
        text: String,
    },
    TextFinished {
        run_id: u64,
        text_id: String,
    },
    ToolStarted {
        run_id: u64,
        call_id: String,
        name: String,
        description: String,
        input: String,
    },
    ToolOutput {
        run_id: u64,
        call_id: String,
        text: String,
    },
    ToolFinished {
        run_id: u64,
        call_id: String,
        name: String,
        title: String,
        output: String,
        diffs: Vec<FileDiff>,
    },
    ToolFailed {
        run_id: u64,
        call_id: String,
        name: String,
        message: String,
    },
    PermissionRequested {
        run_id: u64,
        request_id: u64,
        permission: String,
        patterns: Vec<String>,
        description: String,
    },
    RetryScheduled {
        run_id: u64,
        attempt: u16,
        delay_ms: u64,
        message: String,
    },
    CompactionStarted {
        run_id: u64,
    },
    CompactionFinished {
        run_id: u64,
    },
    CompactionRequired {
        run_id: u64,
    },
    RunError {
        run_id: u64,
        message: String,
    },
    RunFinished {
        run_id: u64,
        outcome: RunOutcome,
    },
}
