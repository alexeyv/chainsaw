use chrono::{DateTime, Utc};
use strum::IntoEnumIterator;

use super::TaskEvent;
use crate::domain::TaskState;

fn timestamp(seconds: i64) -> DateTime<Utc> {
  DateTime::from_timestamp(seconds, 0).unwrap()
}

#[test]
fn constructor_exposes_every_field_without_mutators() {
  let created_at = timestamp(1_700_000_000);
  let event = TaskEvent::new(3, TaskState::InFlight, Some("reuse".to_owned()), created_at).unwrap();

  assert_eq!(event.id(), 3);
  assert_eq!(event.state(), TaskState::InFlight);
  assert_eq!(event.reason(), Some("reuse"));
  assert_eq!(event.created_at(), created_at);
}

#[test]
fn a_reason_is_optional_on_every_transition() {
  let event = TaskEvent::new(1, TaskState::CommittedUnverified, None, timestamp(1)).unwrap();

  assert_eq!(event.reason(), None);
}

#[test]
fn constructor_accepts_every_state_variant() {
  for (index, state) in TaskState::iter().enumerate() {
    assert!(TaskEvent::new(index as i64 + 1, state, None, timestamp(1)).is_ok());
  }
}

#[test]
fn constructor_requires_a_positive_id() {
  for id in [i64::MIN, -1, 0] {
    let error = TaskEvent::new(id, TaskState::Drafted, None, timestamp(1)).unwrap_err();
    assert_eq!(error.to_string(), "id must be positive");
  }
}

#[test]
fn constructor_rejects_a_blank_reason() {
  let error =
    TaskEvent::new(1, TaskState::Aborted, Some("  ".to_owned()), timestamp(1)).unwrap_err();

  assert_eq!(error.to_string(), "reason cannot be blank");
}
