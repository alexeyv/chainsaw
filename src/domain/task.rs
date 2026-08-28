use std::collections::HashSet;
use std::fmt;

use anyhow::{Result, bail};
use strum::EnumIter;

use super::{
  require_nonblank, require_nonnegative, require_optional_nonblank, require_optional_nonnegative,
  require_optional_positive, require_positive, task_event::TaskEvent,
};

/// The task lifecycle: a linear progression that can stop at `Aborted` from any
/// state. Declaration order is the progression order; `Ord` depends on it.
#[derive(Clone, Copy, Debug, EnumIter, Eq, Ord, PartialEq, PartialOrd)]
pub enum TaskState {
  Drafted,
  Dispatched,
  InFlight,
  CommittedUnverified,
  Accepted,
  Aborted,
}

impl TaskState {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Drafted => "drafted",
      Self::Dispatched => "dispatched",
      Self::InFlight => "in_flight",
      Self::CommittedUnverified => "committed_unverified",
      Self::Accepted => "accepted",
      Self::Aborted => "aborted",
    }
  }

  /// A task only moves forward, and only from a state that is not terminal.
  /// `Aborted` sorts last, so it is reachable from every non-terminal state.
  pub fn can_transition_to(self, next: Self) -> bool {
    !self.is_terminal() && next > self
  }

  pub fn is_terminal(self) -> bool {
    matches!(self, Self::Accepted | Self::Aborted)
  }

  fn requires_session(self) -> bool {
    !matches!(self, Self::Drafted | Self::Aborted)
  }

  fn requires_commit(self) -> bool {
    matches!(self, Self::CommittedUnverified | Self::Accepted)
  }
}

impl TryFrom<&str> for TaskState {
  type Error = anyhow::Error;

  fn try_from(value: &str) -> Result<Self> {
    match value {
      "drafted" => Ok(Self::Drafted),
      "dispatched" => Ok(Self::Dispatched),
      "in_flight" => Ok(Self::InFlight),
      "committed_unverified" => Ok(Self::CommittedUnverified),
      "accepted" => Ok(Self::Accepted),
      "aborted" => Ok(Self::Aborted),
      value => bail!("unknown task state {value:?}"),
    }
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
  log_offset: i64,
  base_head: Option<String>,
  predicted_file_list: Option<Vec<String>>,
  is_session_reuse: bool,
  context_size_start: Option<i64>,
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
    log_offset: i64,
    base_head: Option<String>,
    predicted_file_list: Option<Vec<String>>,
    is_session_reuse: bool,
    context_size_start: Option<i64>,
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
    require_nonnegative("log_offset", log_offset)?;
    require_optional_nonblank("base_head", base_head.as_deref())?;
    require_optional_nonnegative("context_size_start", context_size_start)?;
    validate_events(&events)?;
    let state = events.last().expect("validated nonempty event log").state();

    if state.requires_session() && session_id.is_none() {
      bail!("{state:?} task requires a session");
    }
    if state.requires_commit() && commit_sha.is_none() {
      bail!("{state:?} task requires a commit");
    }
    if is_session_reuse && session_id.is_none() {
      bail!("session reuse requires an assigned session");
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
      log_offset,
      base_head,
      predicted_file_list,
      is_session_reuse,
      context_size_start,
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

  /// The reason recorded with the transition into the current state, if any.
  pub fn reason(&self) -> Option<&str> {
    self
      .events
      .last()
      .expect("task event log is never empty")
      .reason()
  }

  /// Transcript byte offset immediately after dispatch. It remains the
  /// measurement baseline when the daemon observes work and takes flight.
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

  pub fn events(&self) -> &[TaskEvent] {
    &self.events
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
  if events[0].state() != TaskState::Drafted {
    bail!("a task's first event must be drafted");
  }
  let mut ids = HashSet::new();
  for event in events {
    if !ids.insert(event.id()) {
      bail!("task events contain duplicate id {}", event.id());
    }
  }
  for pair in events.windows(2) {
    if pair[0].id() >= pair[1].id() {
      bail!(
        "task events are out of identity order: {} follows {}",
        pair[1].id(),
        pair[0].id()
      );
    }
    if !pair[0].state().can_transition_to(pair[1].state()) {
      bail!(
        "task cannot transition from {} to {}",
        pair[0].state(),
        pair[1].state()
      );
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests;
