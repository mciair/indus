//! Permission evaluation and approval coordination for harness tools.

use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use super::{
    event::{HarnessEvent, PermissionReply},
    model::CancellationToken,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionAction {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionRule {
    pub permission: String,
    pub pattern: String,
    pub action: PermissionAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionError {
    Denied,
    Rejected,
    Cancelled,
}

impl fmt::Display for PermissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Denied => "The permission policy denied this tool call.",
            Self::Rejected => "The user rejected this tool call.",
            Self::Cancelled => "The tool call was cancelled while awaiting permission.",
        })
    }
}

impl Error for PermissionError {}

#[derive(Default)]
struct PermissionState {
    rules: Vec<PermissionRule>,
    approved: Vec<PermissionRule>,
    pending: HashMap<u64, Option<PermissionReply>>,
    next_request_id: u64,
    always_approve: bool,
}

#[derive(Clone, Default)]
pub struct PermissionService {
    shared: Arc<(Mutex<PermissionState>, Condvar)>,
}

impl PermissionService {
    pub fn new(rules: Vec<PermissionRule>) -> Self {
        let state = PermissionState {
            rules,
            ..PermissionState::default()
        };
        Self {
            shared: Arc::new((Mutex::new(state), Condvar::new())),
        }
    }

    pub fn evaluate(&self, permission: &str, pattern: &str) -> PermissionAction {
        let (state, _) = &*self.shared;
        let state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        evaluate_rules(
            permission,
            pattern,
            state.rules.iter().chain(state.approved.iter()),
        )
    }

    pub fn set_always_approve(&self, enabled: bool) {
        let (state, _) = &*self.shared;
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .always_approve = enabled;
    }

    pub fn is_always_approve(&self) -> bool {
        let (state, _) = &*self.shared;
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .always_approve
    }

    pub fn authorize(
        &self,
        run_id: u64,
        permission: &str,
        patterns: &[String],
        description: &str,
        cancellation: &CancellationToken,
        emit: &dyn Fn(HarnessEvent),
    ) -> Result<(), PermissionError> {
        let always_approve = self.is_always_approve();
        let mut needs_approval = false;
        for pattern in patterns {
            match self.evaluate(permission, pattern) {
                PermissionAction::Allow => {}
                PermissionAction::Ask if !always_approve => needs_approval = true,
                PermissionAction::Ask => {}
                PermissionAction::Deny => return Err(PermissionError::Denied),
            }
        }
        if !needs_approval {
            return Ok(());
        }

        let request_id = {
            let (state, _) = &*self.shared;
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.next_request_id = state.next_request_id.saturating_add(1);
            let request_id = state.next_request_id;
            state.pending.insert(request_id, None);
            request_id
        };

        emit(HarnessEvent::PermissionRequested {
            run_id,
            request_id,
            permission: permission.to_string(),
            patterns: patterns.to_vec(),
            description: description.to_string(),
        });

        let reply = self.wait_for_reply(request_id, cancellation)?;
        match reply {
            PermissionReply::AllowOnce => Ok(()),
            PermissionReply::AllowAlways => {
                let (state, _) = &*self.shared;
                let mut state = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state
                    .approved
                    .extend(patterns.iter().map(|pattern| PermissionRule {
                        permission: permission.to_string(),
                        pattern: pattern.clone(),
                        action: PermissionAction::Allow,
                    }));
                Ok(())
            }
            PermissionReply::Reject => Err(PermissionError::Rejected),
        }
    }

    pub fn reply(&self, request_id: u64, reply: PermissionReply) -> bool {
        let (state, wake) = &*self.shared;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(slot) = state.pending.get_mut(&request_id) else {
            return false;
        };
        *slot = Some(reply);
        wake.notify_all();
        true
    }

    fn wait_for_reply(
        &self,
        request_id: u64,
        cancellation: &CancellationToken,
    ) -> Result<PermissionReply, PermissionError> {
        let (state, wake) = &*self.shared;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if cancellation.is_cancelled() {
                state.pending.remove(&request_id);
                return Err(PermissionError::Cancelled);
            }
            if let Some(reply) = state.pending.get(&request_id).copied().flatten() {
                state.pending.remove(&request_id);
                return Ok(reply);
            }
            let waited = wake.wait_timeout(state, Duration::from_millis(50));
            state = match waited {
                Ok((state, _)) => state,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }
}

fn evaluate_rules<'a>(
    permission: &str,
    pattern: &str,
    rules: impl DoubleEndedIterator<Item = &'a PermissionRule>,
) -> PermissionAction {
    rules
        .rev()
        .find(|rule| {
            wildcard_match(&rule.permission, permission) && wildcard_match(&rule.pattern, pattern)
        })
        .map_or(PermissionAction::Ask, |rule| rule.action)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut row = vec![false; value.len() + 1];
    row[0] = true;

    for token in pattern {
        let mut next = vec![false; value.len() + 1];
        if token == '*' {
            next[0] = row[0];
            for index in 1..=value.len() {
                next[index] = row[index] || next[index - 1];
            }
        } else {
            for index in 1..=value.len() {
                next[index] = row[index - 1] && (token == '?' || token == value[index - 1]);
            }
        }
        row = next;
    }
    row[value.len()]
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn latest_matching_rule_takes_precedence() {
        let service = PermissionService::new(vec![
            PermissionRule {
                permission: "shell".into(),
                pattern: "*".into(),
                action: PermissionAction::Ask,
            },
            PermissionRule {
                permission: "shell".into(),
                pattern: "git status".into(),
                action: PermissionAction::Allow,
            },
        ]);
        assert_eq!(
            service.evaluate("shell", "git status"),
            PermissionAction::Allow
        );
        assert_eq!(service.evaluate("shell", "git push"), PermissionAction::Ask);
    }

    #[test]
    fn wildcard_matching_supports_multiple_segments() {
        assert!(wildcard_match("src/*.rs", "src/harness/model.rs"));
        assert!(wildcard_match("file-?.txt", "file-a.txt"));
        assert!(!wildcard_match("file-?.txt", "file-long.txt"));
    }

    #[test]
    fn always_approve_bypasses_ask_rules_without_emitting_a_prompt() {
        let service = PermissionService::new(vec![PermissionRule {
            permission: "shell".into(),
            pattern: "*".into(),
            action: PermissionAction::Ask,
        }]);
        service.set_always_approve(true);
        let emitted = Cell::new(false);

        let result = service.authorize(
            1,
            "shell",
            &["git status".into()],
            "Check status",
            &CancellationToken::default(),
            &|_| emitted.set(true),
        );

        assert_eq!(result, Ok(()));
        assert!(!emitted.get());
    }

    #[test]
    fn always_approve_does_not_override_an_explicit_denial() {
        let service = PermissionService::new(vec![PermissionRule {
            permission: "shell".into(),
            pattern: "dangerous".into(),
            action: PermissionAction::Deny,
        }]);
        service.set_always_approve(true);

        assert_eq!(
            service.authorize(
                1,
                "shell",
                &["dangerous".into()],
                "Denied command",
                &CancellationToken::default(),
                &|_| {},
            ),
            Err(PermissionError::Denied)
        );
    }
}
