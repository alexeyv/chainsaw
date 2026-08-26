use std::fmt;

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};

use super::{Calibration, Finding, FindingVerdict, Observation, Task, TaskEvent, TaskState};

pub fn created_at() -> DateTime<Utc> {
  DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

pub fn resolved_at() -> DateTime<Utc> {
  DateTime::from_timestamp(1_700_000_001, 0).unwrap()
}

pub fn finding_from_record(
  id: i64,
  task_id: i64,
  description: &str,
  verdict: Option<FindingVerdict>,
  verdict_reason: Option<&str>,
  fix_task_id: Option<i64>,
  resolved_at: Option<DateTime<Utc>>,
) -> Result<Finding> {
  Finding::from_record(
    id,
    task_id,
    description.to_owned(),
    verdict,
    verdict_reason.map(str::to_owned),
    fix_task_id,
    created_at(),
    resolved_at,
  )
}

pub fn format_finding(finding: &Finding) -> String {
  let verdict = match finding.verdict() {
    Some(verdict) => verdict.to_string(),
    None => "none".to_owned(),
  };
  let verdict_reason = match finding.verdict_reason() {
    Some(reason) => format!("{reason:?}"),
    None => "none".to_owned(),
  };
  let fix_task_id = match finding.fix_task_id() {
    Some(id) => id.to_string(),
    None => "none".to_owned(),
  };
  let created_at = finding
    .created_at()
    .to_rfc3339_opts(SecondsFormat::Secs, true);
  let resolved_at = match finding.resolved_at() {
    Some(time) => time.to_rfc3339_opts(SecondsFormat::Secs, true),
    None => "none".to_owned(),
  };
  format!(
    "id: {}\ntask_id: {}\ndescription: {:?}\nverdict: {}\nverdict_reason: {}\nfix_task_id: {}\ncreated_at: {}\nresolved_at: {}\nis_resolved: {}",
    finding.id(),
    finding.task_id(),
    finding.description(),
    verdict,
    verdict_reason,
    fix_task_id,
    created_at,
    resolved_at,
    finding.is_resolved(),
  )
}

pub fn timestamp(seconds: i64) -> DateTime<Utc> {
  DateTime::from_timestamp(seconds, 0).unwrap()
}

fn format_time(time: DateTime<Utc>) -> String {
  time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn format_option<T: fmt::Display>(value: Option<T>) -> String {
  match value {
    Some(value) => value.to_string(),
    None => "none".to_owned(),
  }
}

fn format_option_text(value: Option<&str>) -> String {
  match value {
    Some(value) => format!("{value:?}"),
    None => "none".to_owned(),
  }
}

/// A calibration for task 7 with every measurement filled in.
pub fn calibration(id: i64, task_id: i64, wall_seconds: Option<f64>) -> Result<Calibration> {
  calibration_measuring(id, task_id, wall_seconds, [2, 20, 4, 35, 100, 900])
}

/// `measurements` are predicted files, predicted lines, actual files, actual
/// lines, context size start, context size end.
pub fn calibration_measuring(
  id: i64,
  task_id: i64,
  wall_seconds: Option<f64>,
  measurements: [i64; 6],
) -> Result<Calibration> {
  let [
    predicted_files,
    predicted_lines,
    actual_files,
    actual_lines,
    start,
    end,
  ] = measurements;
  Calibration::new(
    id,
    task_id,
    predicted_files,
    predicted_lines,
    actual_files,
    actual_lines,
    wall_seconds,
    created_at(),
    start,
    end,
  )
}

pub fn format_calibration(calibration: &Calibration) -> String {
  format!(
    "id: {}\ntask_id: {}\npredicted_files: {}\npredicted_lines: {}\nactual_files: {}\nactual_lines: {}\nwall_seconds: {}\ncreated_at: {}\ncontext_size_start: {}\ncontext_size_end: {}",
    calibration.id(),
    calibration.task_id(),
    calibration.predicted_files(),
    calibration.predicted_lines(),
    calibration.actual_files(),
    calibration.actual_lines(),
    format_option(calibration.wall_seconds()),
    format_time(calibration.created_at()),
    calibration.context_size_start(),
    calibration.context_size_end(),
  )
}

pub fn observation(id: i64, task_id: Option<i64>, text: &str) -> Result<Observation> {
  Observation::new(id, task_id, text.to_owned(), created_at())
}

pub fn format_observation(observation: &Observation) -> String {
  format!(
    "id: {}\ntask_id: {}\ntext: {:?}\ncreated_at: {}",
    observation.id(),
    format_option(observation.task_id()),
    observation.text(),
    format_time(observation.created_at()),
  )
}

/// An event stamped `id` seconds after the shared creation time.
pub fn event(id: i64, state: TaskState, reason: Option<&str>) -> Result<TaskEvent> {
  TaskEvent::new(
    id,
    state,
    reason.map(str::to_owned),
    timestamp(1_700_000_000 + id.clamp(0, 1_000)),
  )
}

pub fn format_event(event: &TaskEvent) -> String {
  format!(
    "id: {}\nstate: {}\nreason: {}\ncreated_at: {}",
    event.id(),
    event.state(),
    format_option_text(event.reason()),
    format_time(event.created_at()),
  )
}

/// The shortest legal event history ending in `state`, with `reason` on the
/// final event. Ids are 1, 2, 3, … in order.
pub fn events_through(state: TaskState, reason: Option<&str>) -> Vec<TaskEvent> {
  let path = match state {
    TaskState::Drafted => vec![TaskState::Drafted],
    TaskState::Dispatched => vec![TaskState::Drafted, TaskState::Dispatched],
    TaskState::InFlight => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
    ],
    TaskState::CommittedUnverified => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::CommittedUnverified,
    ],
    TaskState::Accepted => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::CommittedUnverified,
      TaskState::Accepted,
    ],
    TaskState::Aborted => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::Aborted,
    ],
  };
  let last = path.len();
  path
    .into_iter()
    .zip(1..)
    .map(|(state, id)| event(id, state, if id == last as i64 { reason } else { None }).unwrap())
    .collect()
}

/// Every `Task::new` argument, so a case can override exactly one of them.
pub struct TaskSpec {
  pub id: i64,
  pub text: &'static str,
  pub predicted_files: i64,
  pub predicted_lines: i64,
  pub session_id: Option<i64>,
  pub commit_sha: Option<&'static str>,
  pub created_at: f64,
  pub retry_of_task_id: Option<i64>,
  pub log_offset: i64,
  pub base_head: Option<&'static str>,
  pub predicted_file_list: Option<Vec<&'static str>>,
  pub is_session_reuse: bool,
  pub context_size_start: Option<i64>,
  pub events: Vec<TaskEvent>,
}

/// A drafted task with no session, commit, or file list.
pub fn drafted_task() -> TaskSpec {
  TaskSpec {
    id: 3,
    text: "implement the task",
    predicted_files: 2,
    predicted_lines: 20,
    session_id: None,
    commit_sha: None,
    created_at: 1_700_000_000.5,
    retry_of_task_id: None,
    log_offset: 0,
    base_head: None,
    predicted_file_list: None,
    is_session_reuse: false,
    context_size_start: None,
    events: events_through(TaskState::Drafted, None),
  }
}

/// A task that has been through every state up to and including `state`, with
/// a session and commit assigned whether or not the state needs them.
pub fn task_in(state: TaskState, reason: Option<&str>) -> TaskSpec {
  TaskSpec {
    session_id: Some(7),
    commit_sha: Some("abc123"),
    log_offset: 100,
    base_head: Some("base123"),
    context_size_start: Some(900),
    events: events_through(state, reason),
    ..drafted_task()
  }
}

pub fn build(spec: TaskSpec) -> Result<Task> {
  Task::new(
    spec.id,
    spec.text.to_owned(),
    spec.predicted_files,
    spec.predicted_lines,
    spec.session_id,
    spec.commit_sha.map(str::to_owned),
    spec.created_at,
    spec.retry_of_task_id,
    spec.log_offset,
    spec.base_head.map(str::to_owned),
    spec
      .predicted_file_list
      .map(|files| files.into_iter().map(str::to_owned).collect()),
    spec.is_session_reuse,
    spec.context_size_start,
    spec.events,
  )
}

pub fn format_task(task: &Task) -> String {
  let file_list = match task.predicted_file_list() {
    Some(files) => format!("{files:?}"),
    None => "none".to_owned(),
  };
  let events = task
    .events()
    .iter()
    .map(|event| {
      format!(
        "  {} {} {}",
        event.id(),
        event.state(),
        format_option_text(event.reason())
      )
    })
    .collect::<Vec<_>>()
    .join("\n");
  format!(
    "id: {}\ntext: {:?}\npredicted_files: {}\npredicted_lines: {}\nstate: {}\nsession_id: {}\ncommit_sha: {}\ncreated_at: {:.3}\nretry_of_task_id: {}\nreason: {}\nlog_offset: {}\nbase_head: {}\npredicted_file_list: {}\nis_session_reuse: {}\ncontext_size_start: {}\nevents:\n{}",
    task.id(),
    task.text(),
    task.predicted_files(),
    task.predicted_lines(),
    task.state(),
    format_option(task.session_id()),
    format_option_text(task.commit_sha()),
    task.created_at(),
    format_option(task.retry_of_task_id()),
    format_option_text(task.reason()),
    task.log_offset(),
    format_option_text(task.base_head()),
    file_list,
    task.is_session_reuse(),
    format_option(task.context_size_start()),
    events,
  )
}

pub fn format_tasks(tasks: &[Task]) -> String {
  tasks
    .iter()
    .map(format_task)
    .collect::<Vec<_>>()
    .join("\n\n")
}
