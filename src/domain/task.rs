use std::collections::HashSet;
use std::fmt;

use anyhow::{Result, bail};

use super::task_event::TaskEvent;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState {
  Drafted,
  Dispatched,
  InFlight,
  Committed,
  Verified,
  Accepted,
  Ingested,
  Failed,
}

impl TaskState {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Drafted => "drafted",
      Self::Dispatched => "dispatched",
      Self::InFlight => "in_flight",
      Self::Committed => "committed",
      Self::Verified => "verified",
      Self::Accepted => "accepted",
      Self::Ingested => "ingested",
      Self::Failed => "failed",
    }
  }

  fn requires_session(self) -> bool {
    self != Self::Drafted
  }

  fn requires_commit(self) -> bool {
    matches!(
      self,
      Self::Committed | Self::Verified | Self::Accepted | Self::Ingested
    )
  }

  fn requires_reason(self) -> bool {
    matches!(self, Self::Accepted | Self::Failed)
  }
}

impl fmt::Display for TaskState {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(self.as_str())
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
  id: i64,
  text: String,
  predicted_files: i64,
  predicted_lines: i64,
  session_id: Option<i64>,
  commit_sha: Option<String>,
  created_at: f64,
  retry_of_task_id: Option<i64>,
  reason: Option<String>,
  log_offset: i64,
  base_head: Option<String>,
  predicted_file_list: Option<Vec<String>>,
  is_session_reuse: bool,
  context_size_start: Option<i64>,
  context_size_end: Option<i64>,
  events: Vec<TaskEvent>,
}

impl Task {
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    id: i64,
    text: String,
    predicted_files: i64,
    predicted_lines: i64,
    session_id: Option<i64>,
    commit_sha: Option<String>,
    created_at: f64,
    retry_of_task_id: Option<i64>,
    reason: Option<String>,
    log_offset: i64,
    base_head: Option<String>,
    predicted_file_list: Option<Vec<String>>,
    is_session_reuse: bool,
    context_size_start: Option<i64>,
    context_size_end: Option<i64>,
    events: Vec<TaskEvent>,
  ) -> Result<Self> {
    require_positive("id", id)?;
    require_nonblank("text", &text)?;
    require_nonnegative("predicted_files", predicted_files)?;
    require_nonnegative("predicted_lines", predicted_lines)?;
    require_optional_positive("session_id", session_id)?;
    require_optional_nonblank("commit_sha", commit_sha.as_deref())?;
    if !created_at.is_finite() || created_at < 0.0 {
      bail!("created_at must be finite and nonnegative");
    }
    require_optional_positive("retry_of_task_id", retry_of_task_id)?;
    if retry_of_task_id == Some(id) {
      bail!("a task cannot retry itself");
    }
    require_optional_nonblank("reason", reason.as_deref())?;
    require_nonnegative("log_offset", log_offset)?;
    require_optional_nonblank("base_head", base_head.as_deref())?;
    require_optional_nonnegative("context_size_start", context_size_start)?;
    require_optional_nonnegative("context_size_end", context_size_end)?;
    validate_events(&events)?;
    let state = events.last().expect("validated nonempty event log").state();

    if state.requires_session() && session_id.is_none() {
      bail!("{state:?} task requires a session");
    }
    if state.requires_commit() && commit_sha.is_none() {
      bail!("{state:?} task requires a commit");
    }
    if state.requires_reason() && reason.is_none() {
      bail!("{state:?} task requires a reason");
    }
    if is_session_reuse && session_id.is_none() {
      bail!("session reuse requires an assigned session");
    }
    if context_size_end.is_some() && context_size_start.is_none() {
      bail!("context end requires a context start");
    }
    validate_predicted_files(predicted_files, predicted_file_list.as_deref())?;

    Ok(Self {
      id,
      text,
      predicted_files,
      predicted_lines,
      session_id,
      commit_sha,
      created_at,
      retry_of_task_id,
      reason,
      log_offset,
      base_head,
      predicted_file_list,
      is_session_reuse,
      context_size_start,
      context_size_end,
      events,
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn text(&self) -> &str {
    &self.text
  }

  pub fn predicted_files(&self) -> i64 {
    self.predicted_files
  }

  pub fn predicted_lines(&self) -> i64 {
    self.predicted_lines
  }

  pub fn state(&self) -> TaskState {
    self
      .events
      .last()
      .expect("task event log is never empty")
      .state()
  }

  pub fn session_id(&self) -> Option<i64> {
    self.session_id
  }

  pub fn commit_sha(&self) -> Option<&str> {
    self.commit_sha.as_deref()
  }

  pub fn created_at(&self) -> f64 {
    self.created_at
  }

  pub fn retry_of_task_id(&self) -> Option<i64> {
    self.retry_of_task_id
  }

  pub fn reason(&self) -> Option<&str> {
    self.reason.as_deref()
  }

  pub fn log_offset(&self) -> i64 {
    self.log_offset
  }

  pub fn base_head(&self) -> Option<&str> {
    self.base_head.as_deref()
  }

  pub fn predicted_file_list(&self) -> Option<&[String]> {
    self.predicted_file_list.as_deref()
  }

  pub fn is_session_reuse(&self) -> bool {
    self.is_session_reuse
  }

  pub fn context_size_start(&self) -> Option<i64> {
    self.context_size_start
  }

  pub fn context_size_end(&self) -> Option<i64> {
    self.context_size_end
  }

  pub fn events(&self) -> &[TaskEvent] {
    &self.events
  }
}

fn require_positive(field: &'static str, value: i64) -> Result<()> {
  if value <= 0 {
    bail!("{field} must be positive");
  } else {
    Ok(())
  }
}

fn require_optional_positive(field: &'static str, value: Option<i64>) -> Result<()> {
  match value {
    Some(value) => require_positive(field, value),
    None => Ok(()),
  }
}

fn require_nonnegative(field: &'static str, value: i64) -> Result<()> {
  if value < 0 {
    bail!("{field} cannot be negative");
  } else {
    Ok(())
  }
}

fn require_optional_nonnegative(field: &'static str, value: Option<i64>) -> Result<()> {
  match value {
    Some(value) => require_nonnegative(field, value),
    None => Ok(()),
  }
}

fn require_nonblank(field: &'static str, value: &str) -> Result<()> {
  if value.trim().is_empty() {
    bail!("{field} cannot be blank");
  } else {
    Ok(())
  }
}

fn require_optional_nonblank(field: &'static str, value: Option<&str>) -> Result<()> {
  match value {
    Some(value) => require_nonblank(field, value),
    None => Ok(()),
  }
}

fn validate_predicted_files(
  predicted_files: i64,
  predicted_file_list: Option<&[String]>,
) -> Result<()> {
  let Some(files) = predicted_file_list else {
    return Ok(());
  };
  if predicted_files != i64::try_from(files.len()).unwrap_or(i64::MAX) {
    bail!(
      "predicted file count {predicted_files} does not match {} listed files",
      files.len()
    );
  }
  let mut unique = HashSet::new();
  for file in files {
    require_nonblank("predicted_file_list", file)?;
    if !unique.insert(file) {
      bail!("predicted file list contains duplicate {file:?}");
    }
  }
  Ok(())
}

fn validate_events(events: &[TaskEvent]) -> Result<()> {
  if events.is_empty() {
    bail!("a task requires at least one event");
  }
  let mut ids = HashSet::new();
  for event in events {
    if !ids.insert(event.id()) {
      bail!("task events contain duplicate id {}", event.id());
    }
  }
  for pair in events.windows(2) {
    if pair[0].created_at() > pair[1].created_at() {
      bail!(
        "task event {} occurs before event {}",
        pair[1].id(),
        pair[0].id()
      );
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use anyhow::Result;
  use chrono::{DateTime, Utc};

  use super::{Task, TaskState};
  use crate::domain::TaskEvent;

  fn timestamp(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).unwrap()
  }

  #[test]
  fn states_display_with_their_stable_names() {
    for (state, name) in [
      (TaskState::Drafted, "drafted"),
      (TaskState::Dispatched, "dispatched"),
      (TaskState::InFlight, "in_flight"),
      (TaskState::Committed, "committed"),
      (TaskState::Verified, "verified"),
      (TaskState::Accepted, "accepted"),
      (TaskState::Ingested, "ingested"),
      (TaskState::Failed, "failed"),
    ] {
      assert_eq!(state.to_string(), state.as_str());
      assert_eq!(state.to_string(), name);
    }
  }

  #[allow(clippy::too_many_arguments)]
  fn task_with(
    id: i64,
    text: &str,
    predicted_files: i64,
    state: TaskState,
    session_id: Option<i64>,
    commit_sha: Option<&str>,
    reason: Option<&str>,
    file_list: Option<Vec<&str>>,
    is_session_reuse: bool,
    context_size_start: Option<i64>,
    context_size_end: Option<i64>,
  ) -> Result<Task> {
    Task::new(
      id,
      text.to_owned(),
      predicted_files,
      20,
      session_id,
      commit_sha.map(str::to_owned),
      1_700_000_000.0,
      None,
      reason.map(str::to_owned),
      100,
      Some("abc123".to_owned()),
      file_list.map(|files| files.into_iter().map(str::to_owned).collect()),
      is_session_reuse,
      context_size_start,
      context_size_end,
      vec![TaskEvent::new(1, state, timestamp(1_700_000_000)).unwrap()],
    )
  }

  fn drafted_task_with_events(events: Vec<TaskEvent>) -> Result<Task> {
    Task::new(
      1,
      "task".to_owned(),
      0,
      0,
      None,
      None,
      1_700_000_000.0,
      None,
      None,
      0,
      None,
      None,
      false,
      None,
      None,
      events,
    )
  }

  #[test]
  fn exposes_every_field_without_mutators() {
    let events = vec![
      TaskEvent::new(1, TaskState::Drafted, timestamp(1_699_999_900)).unwrap(),
      TaskEvent::new(2, TaskState::InFlight, timestamp(1_700_000_000)).unwrap(),
    ];
    let task = Task::new(
      2,
      "Implement the task".to_owned(),
      2,
      20,
      Some(7),
      None,
      1_700_000_000.0,
      Some(1),
      Some("retrying".to_owned()),
      100,
      Some("abc123".to_owned()),
      Some(vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()]),
      true,
      Some(40),
      Some(75),
      events.clone(),
    )
    .unwrap();

    assert_eq!(task.id(), 2);
    assert_eq!(task.text(), "Implement the task");
    assert_eq!(task.predicted_files(), 2);
    assert_eq!(task.predicted_lines(), 20);
    assert_eq!(task.state(), TaskState::InFlight);
    assert_eq!(task.session_id(), Some(7));
    assert_eq!(task.commit_sha(), None);
    assert_eq!(task.created_at(), 1_700_000_000.0);
    assert_eq!(task.retry_of_task_id(), Some(1));
    assert_eq!(task.reason(), Some("retrying"));
    assert_eq!(task.log_offset(), 100);
    assert_eq!(task.base_head(), Some("abc123"));
    assert_eq!(
      task.predicted_file_list(),
      Some(["src/a.rs".to_owned(), "src/b.rs".to_owned()].as_slice())
    );
    assert!(task.is_session_reuse());
    assert_eq!(task.context_size_start(), Some(40));
    assert_eq!(task.context_size_end(), Some(75));
    assert_eq!(task.events(), events.as_slice());
  }

  #[test]
  fn requires_a_positive_identity() {
    let error = task_with(
      0,
      "task",
      0,
      TaskState::Drafted,
      None,
      None,
      None,
      None,
      false,
      None,
      None,
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "id must be positive");
  }

  #[test]
  fn requires_nonblank_text_and_nonnegative_estimates() {
    let blank = task_with(
      1,
      "  ",
      0,
      TaskState::Drafted,
      None,
      None,
      None,
      None,
      false,
      None,
      None,
    )
    .unwrap_err();
    let negative = task_with(
      1,
      "task",
      -1,
      TaskState::Drafted,
      None,
      None,
      None,
      None,
      false,
      None,
      None,
    )
    .unwrap_err();

    assert_eq!(blank.to_string(), "text cannot be blank");
    assert_eq!(negative.to_string(), "predicted_files cannot be negative");
  }

  #[test]
  fn assigned_states_require_a_session() {
    let error = task_with(
      1,
      "task",
      0,
      TaskState::InFlight,
      None,
      None,
      None,
      None,
      false,
      None,
      None,
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "InFlight task requires a session");
  }

  #[test]
  fn completed_states_require_a_commit() {
    let error = task_with(
      1,
      "task",
      0,
      TaskState::Verified,
      Some(2),
      None,
      None,
      None,
      false,
      None,
      None,
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "Verified task requires a commit");
  }

  #[test]
  fn failed_and_accepted_states_require_a_reason() {
    let error = task_with(
      1,
      "task",
      0,
      TaskState::Failed,
      Some(2),
      None,
      None,
      None,
      false,
      None,
      None,
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "Failed task requires a reason");
  }

  #[test]
  fn a_task_cannot_retry_itself() {
    let error = Task::new(
      1,
      "task".to_owned(),
      0,
      0,
      None,
      None,
      1.0,
      Some(1),
      None,
      0,
      None,
      None,
      false,
      None,
      None,
      vec![TaskEvent::new(1, TaskState::Drafted, timestamp(1)).unwrap()],
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "a task cannot retry itself");
  }

  #[test]
  fn predicted_file_count_must_match_the_list() {
    let error = task_with(
      1,
      "task",
      1,
      TaskState::Drafted,
      None,
      None,
      None,
      Some(vec!["a.rs", "b.rs"]),
      false,
      None,
      None,
    )
    .unwrap_err();

    assert_eq!(
      error.to_string(),
      "predicted file count 1 does not match 2 listed files"
    );
  }

  #[test]
  fn predicted_file_list_rejects_blank_and_duplicate_paths() {
    let blank = task_with(
      1,
      "task",
      1,
      TaskState::Drafted,
      None,
      None,
      None,
      Some(vec![" "]),
      false,
      None,
      None,
    )
    .unwrap_err();
    let duplicate = task_with(
      1,
      "task",
      2,
      TaskState::Drafted,
      None,
      None,
      None,
      Some(vec!["a.rs", "a.rs"]),
      false,
      None,
      None,
    )
    .unwrap_err();

    assert_eq!(blank.to_string(), "predicted_file_list cannot be blank");
    assert_eq!(
      duplicate.to_string(),
      "predicted file list contains duplicate \"a.rs\""
    );
  }

  #[test]
  fn session_reuse_requires_a_session() {
    let error = task_with(
      1,
      "task",
      0,
      TaskState::Drafted,
      None,
      None,
      None,
      None,
      true,
      None,
      None,
    )
    .unwrap_err();

    assert_eq!(
      error.to_string(),
      "session reuse requires an assigned session"
    );
  }

  #[test]
  fn context_end_requires_a_start() {
    let error = task_with(
      1,
      "task",
      0,
      TaskState::Drafted,
      None,
      None,
      None,
      None,
      false,
      None,
      Some(50),
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "context end requires a context start");
  }

  #[test]
  fn requires_at_least_one_event() {
    let error = drafted_task_with_events(Vec::new()).unwrap_err();

    assert_eq!(error.to_string(), "a task requires at least one event");
  }

  #[test]
  fn event_ids_are_unique_within_a_task() {
    let error = drafted_task_with_events(vec![
      TaskEvent::new(1, TaskState::Drafted, timestamp(1)).unwrap(),
      TaskEvent::new(1, TaskState::Drafted, timestamp(2)).unwrap(),
    ])
    .unwrap_err();

    assert_eq!(error.to_string(), "task events contain duplicate id 1");
  }

  #[test]
  fn events_are_in_nondecreasing_timestamp_order() {
    let error = drafted_task_with_events(vec![
      TaskEvent::new(1, TaskState::Drafted, timestamp(2)).unwrap(),
      TaskEvent::new(2, TaskState::Drafted, timestamp(1)).unwrap(),
    ])
    .unwrap_err();

    assert_eq!(error.to_string(), "task event 2 occurs before event 1");
  }
}
