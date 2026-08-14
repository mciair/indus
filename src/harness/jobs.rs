//! Persistent scheduled work managed by the Indus harness.

use std::{
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use serde::{Deserialize, Serialize};

use super::classifier::{Classification, JobSchedule};

const STORE_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Active,
    Paused,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub schedule: JobSchedule,
    pub status: JobStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub next_run_at: Option<u64>,
    pub last_started_at: Option<u64>,
    pub last_completed_at: Option<u64>,
    pub run_count: u64,
    pub last_result: Option<String>,
    pub last_error: Option<String>,
    pub classifier_decision: Classification,
}

impl Job {
    pub fn schedule_description(&self) -> String {
        match &self.schedule {
            JobSchedule::Interval { interval_ms } => {
                format!("every {}", duration_label(*interval_ms))
            }
            JobSchedule::ClockBased {
                clock_times,
                time_zone,
            } => format!(
                "daily at {} ({})",
                clock_times.join(", "),
                time_zone.as_deref().unwrap_or("local time")
            ),
            JobSchedule::Cron { cron_expr } => format!("cron {cron_expr}"),
            JobSchedule::Continuous => "continuously".to_string(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoredJobs {
    #[serde(default = "store_version")]
    version: u8,
    #[serde(default)]
    jobs: Vec<Job>,
}

fn store_version() -> u8 {
    STORE_VERSION
}

#[derive(Clone)]
pub struct JobService {
    state: Arc<Mutex<JobState>>,
}

struct JobState {
    path: Option<PathBuf>,
    stored: StoredJobs,
    sequence: u64,
}

impl JobService {
    pub fn load() -> Self {
        Self::from_path(job_store_path())
    }

    fn from_path(path: Option<PathBuf>) -> Self {
        let stored = path
            .as_ref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<StoredJobs>(&bytes).ok())
            .filter(|stored| stored.version == STORE_VERSION)
            .unwrap_or_else(|| StoredJobs {
                version: STORE_VERSION,
                jobs: Vec::new(),
            });
        Self {
            state: Arc::new(Mutex::new(JobState {
                path,
                sequence: stored.jobs.len() as u64,
                stored,
            })),
        }
    }

    pub fn create(&self, goal: impl Into<String>, decision: Classification) -> io::Result<Job> {
        let goal = goal.into();
        let schedule = decision.schedule.clone().unwrap_or(JobSchedule::Continuous);
        let now = now_ms();
        let mut state = self.lock();
        state.sequence = state.sequence.saturating_add(1);
        let job = Job {
            id: format!("job-{now}-{}", state.sequence),
            name: decision.short_description.clone(),
            goal,
            schedule: schedule.clone(),
            status: JobStatus::Active,
            created_at: now,
            updated_at: now,
            next_run_at: next_run_at(&schedule, now),
            last_started_at: None,
            last_completed_at: None,
            run_count: 0,
            last_result: None,
            last_error: None,
            classifier_decision: decision,
        };
        state.stored.jobs.push(job.clone());
        persist(&state)?;
        Ok(job)
    }

    pub fn list(&self) -> Vec<Job> {
        self.lock().stored.jobs.clone()
    }

    pub fn get(&self, id: &str) -> Option<Job> {
        self.lock()
            .stored
            .jobs
            .iter()
            .find(|job| job.id == id)
            .cloned()
    }

    pub fn due(&self, now: u64) -> Vec<Job> {
        self.lock()
            .stored
            .jobs
            .iter()
            .filter(|job| {
                job.status == JobStatus::Active
                    && job.next_run_at.is_some_and(|next| next <= now)
                    && job
                        .last_started_at
                        .zip(job.last_completed_at)
                        .is_none_or(|(started, completed)| completed >= started)
            })
            .cloned()
            .collect()
    }

    pub fn mark_started(&self, id: &str, at: u64) -> io::Result<Option<Job>> {
        self.update(id, |job| {
            if job.status != JobStatus::Active {
                return false;
            }
            job.last_started_at = Some(at);
            job.next_run_at = None;
            job.last_error = None;
            true
        })
    }

    pub fn mark_completed(
        &self,
        id: &str,
        at: u64,
        result: impl Into<String>,
    ) -> io::Result<Option<Job>> {
        let result = result.into();
        self.update(id, move |job| {
            job.last_completed_at = Some(at);
            job.run_count = job.run_count.saturating_add(1);
            job.last_result = Some(result);
            job.last_error = None;
            job.next_run_at = (job.status == JobStatus::Active)
                .then(|| next_run_at(&job.schedule, at))
                .flatten();
            true
        })
    }

    pub fn mark_failed(
        &self,
        id: &str,
        at: u64,
        message: impl Into<String>,
    ) -> io::Result<Option<Job>> {
        let message = message.into();
        self.update(id, move |job| {
            job.last_completed_at = Some(at);
            job.run_count = job.run_count.saturating_add(1);
            job.last_error = Some(message);
            job.next_run_at = (job.status == JobStatus::Active)
                .then(|| next_run_at(&job.schedule, at))
                .flatten();
            true
        })
    }

    pub fn pause(&self, id: &str) -> io::Result<Option<Job>> {
        self.update(id, |job| {
            job.status = JobStatus::Paused;
            job.next_run_at = None;
            true
        })
    }

    pub fn resume(&self, id: &str) -> io::Result<Option<Job>> {
        let now = now_ms();
        self.update(id, move |job| {
            job.status = JobStatus::Active;
            job.next_run_at = next_run_at(&job.schedule, now);
            true
        })
    }

    pub fn complete(&self, id: &str) -> io::Result<Option<Job>> {
        self.update(id, |job| {
            job.status = JobStatus::Completed;
            job.next_run_at = None;
            true
        })
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut Job) -> bool) -> io::Result<Option<Job>> {
        let mut state = self.lock();
        let result = {
            let Some(job) = state.stored.jobs.iter_mut().find(|job| job.id == id) else {
                return Ok(None);
            };
            if !change(job) {
                return Ok(None);
            }
            job.updated_at = now_ms();
            job.clone()
        };
        persist(&state)?;
        Ok(Some(result))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, JobState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        Self::from_path(Some(path))
    }
}

impl Default for JobService {
    fn default() -> Self {
        Self::load()
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn next_run_at(schedule: &JobSchedule, from_ms: u64) -> Option<u64> {
    match schedule {
        JobSchedule::Interval { interval_ms } => Some(from_ms.saturating_add(*interval_ms)),
        JobSchedule::Continuous => Some(from_ms.saturating_add(1_000)),
        JobSchedule::ClockBased {
            clock_times,
            time_zone,
        } => next_clock_run(clock_times, time_zone.as_deref(), from_ms),
        JobSchedule::Cron { cron_expr } => next_cron_run(cron_expr, from_ms),
    }
}

fn next_clock_run(times: &[String], zone: Option<&str>, from_ms: u64) -> Option<u64> {
    let zone = zone
        .and_then(|zone| Tz::from_str(zone).ok())
        .or_else(|| {
            std::env::var("TZ")
                .ok()
                .and_then(|zone| Tz::from_str(&zone).ok())
        })
        .unwrap_or(chrono_tz::Asia::Kolkata);
    let from = Utc
        .timestamp_millis_opt(from_ms as i64)
        .single()?
        .with_timezone(&zone);
    let date = from.date_naive();
    for day_offset in 0..=2 {
        let date = date.checked_add_days(chrono::Days::new(day_offset))?;
        for time in times {
            let (hour, minute) = parse_clock(time)?;
            let local = date.and_hms_opt(hour, minute, 0)?;
            let candidate = zone
                .from_local_datetime(&local)
                .earliest()
                .or_else(|| zone.from_local_datetime(&local).latest())?;
            let timestamp = candidate.with_timezone(&Utc).timestamp_millis();
            if timestamp > from_ms as i64 {
                return Some(timestamp as u64);
            }
        }
    }
    None
}

fn next_cron_run(expression: &str, from_ms: u64) -> Option<u64> {
    let fields = expression.split_whitespace().count();
    let expression = if fields == 5 {
        format!("0 {expression}")
    } else {
        expression.to_string()
    };
    let schedule = Schedule::from_str(&expression).ok()?;
    let from = Utc.timestamp_millis_opt(from_ms as i64).single()?;
    schedule
        .after(&from)
        .next()
        .map(|date| date.timestamp_millis() as u64)
}

fn parse_clock(value: &str) -> Option<(u32, u32)> {
    let mut parts = value.split(':');
    let hour = parts.next()?.parse().ok()?;
    let minute = parts.next()?.parse().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 {
        return None;
    }
    Some((hour, minute))
}

fn duration_label(milliseconds: u64) -> String {
    if milliseconds.is_multiple_of(3_600_000) {
        format!("{}h", milliseconds / 3_600_000)
    } else if milliseconds.is_multiple_of(60_000) {
        format!("{}m", milliseconds / 60_000)
    } else if milliseconds.is_multiple_of(1_000) {
        format!("{}s", milliseconds / 1_000)
    } else {
        format!("{milliseconds}ms")
    }
}

fn job_store_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .map(|root| root.join("indus").join("jobs.json"))
}

fn persist(state: &JobState) -> io::Result<()> {
    let Some(path) = &state.path else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Jobs path has no parent"))?;
    fs::create_dir_all(parent)?;
    secure_directory(parent)?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&state.stored).map_err(io::Error::other)?;
    write_private_file(&temporary, &bytes)?;
    fs::rename(&temporary, path)?;
    secure_file(path)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn secure_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::classifier::GoalCategory;

    fn decision(schedule: JobSchedule) -> Classification {
        Classification {
            category: GoalCategory::TimeBasedJob,
            estimated_tool_calls: 5,
            estimated_minutes: 10.0,
            has_clear_endpoint: false,
            is_recurring: true,
            confidence: 0.9,
            routing_reason: "recurring request".into(),
            short_description: "Monitor deployment health".into(),
            schedule: Some(schedule),
        }
    }

    #[test]
    fn jobs_persist_across_service_instances() {
        let root = std::env::temp_dir().join(format!("indus-jobs-test-{}", now_ms()));
        let path = root.join("jobs.json");
        let first = JobService::at(path.clone());
        let created = first
            .create(
                "monitor deployment",
                decision(JobSchedule::Interval {
                    interval_ms: 60_000,
                }),
            )
            .unwrap();
        let second = JobService::at(path);
        assert_eq!(second.get(&created.id).unwrap().goal, "monitor deployment");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interval_jobs_advance_after_completion() {
        let service = JobService::from_path(None);
        let job = service
            .create(
                "monitor deployment",
                decision(JobSchedule::Interval { interval_ms: 5_000 }),
            )
            .unwrap();
        service.mark_started(&job.id, 10_000).unwrap();
        let completed = service
            .mark_completed(&job.id, 12_000, "healthy")
            .unwrap()
            .unwrap();
        assert_eq!(completed.next_run_at, Some(17_000));
        assert_eq!(completed.run_count, 1);
    }

    #[test]
    fn five_field_cron_expressions_are_supported() {
        assert!(next_cron_run("*/5 * * * *", 0).is_some());
    }
}
