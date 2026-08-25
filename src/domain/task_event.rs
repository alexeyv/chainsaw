use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use super::task::TaskState;

#[derive(Clone, Debug, PartialEq)]
pub struct TaskEvent {
  id: i64,
  state: TaskState,
  created_at: DateTime<Utc>,
}

impl TaskEvent {
  pub fn new(id: i64, state: TaskState, created_at: DateTime<Utc>) -> Result<Self> {
    if id <= 0 {
      bail!("id must be positive");
    }

    Ok(Self {
      id,
      state,
      created_at,
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn state(&self) -> TaskState {
    self.state
  }

  pub fn created_at(&self) -> DateTime<Utc> {
    self.created_at
  }
}

#[cfg(test)]
mod tests {
  use chrono::{DateTime, Utc};

  use super::TaskEvent;
  use crate::domain::TaskState;

  fn timestamp(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).unwrap()
  }

  #[test]
  fn constructor_exposes_every_field_without_mutators() {
    let created_at = timestamp(1_700_000_000);
    let event = TaskEvent::new(3, TaskState::InFlight, created_at).unwrap();

    assert_eq!(event.id(), 3);
    assert_eq!(event.state(), TaskState::InFlight);
    assert_eq!(event.created_at(), created_at);
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
      assert!(TaskEvent::new(index as i64 + 1, state, timestamp(1)).is_ok());
    }
  }

  #[test]
  fn constructor_requires_a_positive_id() {
    for id in [i64::MIN, -1, 0] {
      let error = TaskEvent::new(id, TaskState::Drafted, timestamp(1)).unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }
}
