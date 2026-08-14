//! Hidden preflight classification for routing direct work and persistent Jobs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{
    CancellationToken, ModelContent, ModelEvent, ModelMessage, ModelRequest, ModelTransport, Role,
    TransportError,
};

const CLASSIFIER_SYSTEM: &str = r#"Classify the user's incoming goal before execution.
Do not solve the goal. Return only one JSON object without Markdown or prose.

JSON shape:
{
  "category": "answer_based" | "tasking_goal" | "time_based_job",
  "estimated_tool_calls": number,
  "estimated_minutes": number,
  "has_clear_endpoint": boolean,
  "is_recurring": boolean,
  "confidence": number,
  "routing_reason": string,
  "short_description": string,
  "schedule": { "type": "interval", "interval_ms": number }
            | { "type": "clock_based", "clock_times": ["HH:mm"], "time_zone": "IANA timezone optional" }
            | { "type": "cron", "cron_expr": string }
            | { "type": "24_7" }
}

Routing policy:
- answer_based: ordinary questions and work expected to need fewer than 3 tool calls or under 1 minute.
- tasking_goal: multi-step implementation, investigation, testing, debugging, review, or other work with a clear endpoint.
- time_based_job: scheduled, recurring, monitored, continuous, or explicitly long-running work with no single immediate endpoint.
- Include schedule only for time_based_job. Never invent a cadence. Use 24_7 when continuous work has no concrete cadence.
- Confidence is from 0 to 1. short_description is 3 to 8 words.
- When uncertain, choose tasking_goal."#;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalCategory {
    AnswerBased,
    TaskingGoal,
    TimeBasedJob,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobSchedule {
    Interval {
        interval_ms: u64,
    },
    ClockBased {
        clock_times: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time_zone: Option<String>,
    },
    Cron {
        cron_expr: String,
    },
    #[serde(rename = "24_7")]
    Continuous,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Classification {
    pub category: GoalCategory,
    pub estimated_tool_calls: u32,
    pub estimated_minutes: f64,
    pub has_clear_endpoint: bool,
    pub is_recurring: bool,
    pub confidence: f64,
    pub routing_reason: String,
    pub short_description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<JobSchedule>,
}

impl Classification {
    pub fn fallback(reason: impl Into<String>) -> Self {
        Self {
            category: GoalCategory::AnswerBased,
            estimated_tool_calls: 1,
            estimated_minutes: 1.0,
            has_clear_endpoint: true,
            is_recurring: false,
            confidence: 0.0,
            routing_reason: reason.into(),
            short_description: "Direct response".to_string(),
            schedule: None,
        }
    }

    fn normalize(mut self) -> Self {
        self.confidence = self.confidence.clamp(0.0, 1.0);
        self.estimated_minutes = self.estimated_minutes.max(0.0);
        self.short_description = normalize_description(&self.short_description);
        self.routing_reason = self.routing_reason.trim().to_string();

        if self.confidence < 0.65 {
            self.category = GoalCategory::AnswerBased;
            self.schedule = None;
            return self;
        }
        if self.category != GoalCategory::TimeBasedJob
            && !self.is_recurring
            && (self.estimated_tool_calls < 3 || self.estimated_minutes < 1.0)
        {
            self.category = GoalCategory::AnswerBased;
            self.schedule = None;
            return self;
        }
        if self.category == GoalCategory::TimeBasedJob {
            self.is_recurring = true;
            self.schedule = Some(normalize_schedule(self.schedule));
        } else {
            self.schedule = None;
        }
        self
    }
}

pub fn classify(
    transport: &dyn ModelTransport,
    goal: &str,
    cancellation: &CancellationToken,
) -> Result<Classification, TransportError> {
    let request = ModelRequest {
        system: vec![CLASSIFIER_SYSTEM.to_string()],
        messages: vec![ModelMessage {
            role: Role::User,
            content: vec![ModelContent::Text(format!("Goal:\n{}", goal.trim()))],
        }],
        tools: Vec::new(),
        step: 0,
    };
    let mut text = String::new();
    transport.stream(
        request,
        &mut |event| {
            if let ModelEvent::TextDelta { text: delta, .. } = event {
                text.push_str(&delta);
            }
            Ok(())
        },
        cancellation,
    )?;
    decode(&text)
        .map(Classification::normalize)
        .map_err(|message| {
            TransportError::fatal(format!(
                "The classifier returned an invalid decision: {message}"
            ))
        })
}

fn decode(text: &str) -> Result<Classification, String> {
    let value = extract_json(text).ok_or_else(|| "no JSON object was found".to_string())?;
    serde_json::from_str(value).map_err(|error| error.to_string())
}

fn extract_json(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (end >= start).then_some(&trimmed[start..=end])
}

fn normalize_description(value: &str) -> String {
    let words: Vec<&str> = value
        .split_whitespace()
        .filter(|word| !word.is_empty())
        .take(8)
        .collect();
    if words.len() >= 3 {
        words.join(" ")
    } else {
        "Background scheduled work".to_string()
    }
}

fn normalize_schedule(schedule: Option<JobSchedule>) -> JobSchedule {
    match schedule {
        Some(JobSchedule::Interval { interval_ms }) => JobSchedule::Interval {
            interval_ms: interval_ms.max(1_000),
        },
        Some(JobSchedule::ClockBased {
            clock_times,
            time_zone,
        }) => {
            let mut times: Vec<String> = clock_times
                .into_iter()
                .filter_map(|time| normalize_clock_time(&time))
                .collect();
            times.sort();
            times.dedup();
            if times.is_empty() {
                JobSchedule::Continuous
            } else {
                JobSchedule::ClockBased {
                    clock_times: times,
                    time_zone: time_zone.filter(|zone| !zone.trim().is_empty()),
                }
            }
        }
        Some(JobSchedule::Cron { cron_expr }) if !cron_expr.trim().is_empty() => {
            JobSchedule::Cron {
                cron_expr: cron_expr.trim().to_string(),
            }
        }
        Some(JobSchedule::Cron { .. }) | Some(JobSchedule::Continuous) | None => {
            JobSchedule::Continuous
        }
    }
}

fn normalize_clock_time(value: &str) -> Option<String> {
    let mut parts = value.trim().split(':');
    let hour = parts.next()?.parse::<u8>().ok()?;
    let minute = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 {
        return None;
    }
    Some(format!("{hour:02}:{minute:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::model::{StopReason, Usage};

    struct ClassificationTransport(&'static str);

    impl ModelTransport for ClassificationTransport {
        fn stream(
            &self,
            _request: ModelRequest,
            on_event: &mut dyn FnMut(ModelEvent) -> Result<(), TransportError>,
            _cancellation: &CancellationToken,
        ) -> Result<(), TransportError> {
            on_event(ModelEvent::TextStarted { id: "text".into() })?;
            on_event(ModelEvent::TextDelta {
                id: "text".into(),
                text: self.0.into(),
            })?;
            on_event(ModelEvent::TextFinished { id: "text".into() })?;
            on_event(ModelEvent::StepFinished {
                reason: StopReason::Stop,
                usage: Usage::default(),
            })
        }
    }

    #[test]
    fn classifier_extracts_json_from_wrapping_text() {
        let value = r#"result: {"category":"tasking_goal","estimated_tool_calls":8,"estimated_minutes":12,"has_clear_endpoint":true,"is_recurring":false,"confidence":0.9,"routing_reason":"implementation","short_description":"Implement provider transport layer"}"#;
        let decision = classify(
            &ClassificationTransport(value),
            "build",
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(decision.category, GoalCategory::TaskingGoal);
    }

    #[test]
    fn low_confidence_decisions_fall_back_to_direct_execution() {
        let decision = Classification {
            category: GoalCategory::TimeBasedJob,
            estimated_tool_calls: 20,
            estimated_minutes: 30.0,
            has_clear_endpoint: false,
            is_recurring: true,
            confidence: 0.4,
            routing_reason: "uncertain".into(),
            short_description: "Watch the deployment continuously".into(),
            schedule: Some(JobSchedule::Continuous),
        }
        .normalize();
        assert_eq!(decision.category, GoalCategory::AnswerBased);
        assert!(decision.schedule.is_none());
    }

    #[test]
    fn invalid_clock_entries_are_removed() {
        let schedule = normalize_schedule(Some(JobSchedule::ClockBased {
            clock_times: vec!["7:05".into(), "25:00".into(), "07:05".into()],
            time_zone: None,
        }));
        assert_eq!(
            schedule,
            JobSchedule::ClockBased {
                clock_times: vec!["07:05".into()],
                time_zone: None
            }
        );
    }
}
