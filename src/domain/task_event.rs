use std::error::Error;
use std::fmt;

use super::task::TaskState;

#[derive(Clone, Debug, PartialEq)]
pub struct TaskEvent {
  id: i64,
  state: TaskState,
  at: f64,
}

impl TaskEvent {
  pub fn new(id: i64, state: TaskState, at: f64) -> Result<Self, TaskEventError> {
    if id <= 0 {
      return Err(TaskEventError::NonPositiveId);
    }
    if !at.is_finite() || at < 0.0 {
      return Err(TaskEventError::InvalidTimestamp);
    }

    Ok(Self { id, state, at })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn state(&self) -> TaskState {
    self.state
  }

  pub fn at(&self) -> f64 {
    self.at
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskEventError {
  NonPositiveId,
  InvalidTimestamp,
}

impl fmt::Display for TaskEventError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::NonPositiveId => write!(formatter, "id must be positive"),
      Self::InvalidTimestamp => write!(formatter, "at must be finite and nonnegative"),
    }
  }
}

impl Error for TaskEventError {}

#[cfg(test)]
mod tests {
  use super::{TaskEvent, TaskEventError};
  use crate::domain::TaskState;

  #[test]
  fn constructor_exposes_every_field_without_mutators() {
    let event = TaskEvent::new(3, TaskState::InFlight, 1_700_000_000.0).unwrap();

    assert_eq!(event.id(), 3);
    assert_eq!(event.state(), TaskState::InFlight);
    assert_eq!(event.at(), 1_700_000_000.0);
  }

  #[test]
  fn constructor_accepts_every_state_variant() {
    let states = [
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::Committed,
      TaskState::Verified,
      TaskState::Accepted,
      TaskState::Ingested,
      TaskState::Failed,
    ];

    for (index, state) in states.into_iter().enumerate() {
      assert!(TaskEvent::new(index as i64 + 1, state, 1.0).is_ok());
    }
  }

  #[test]
  fn constructor_requires_a_positive_id() {
    for id in [i64::MIN, -1, 0] {
      let error = TaskEvent::new(id, TaskState::Drafted, 1.0).unwrap_err();
      assert_eq!(error, TaskEventError::NonPositiveId);
    }
  }

  #[test]
  fn constructor_requires_a_finite_nonnegative_timestamp() {
    for at in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
      let error = TaskEvent::new(1, TaskState::Drafted, at).unwrap_err();
      assert_eq!(error, TaskEventError::InvalidTimestamp);
    }

    assert!(TaskEvent::new(1, TaskState::Drafted, 0.0).is_ok());
  }
}
