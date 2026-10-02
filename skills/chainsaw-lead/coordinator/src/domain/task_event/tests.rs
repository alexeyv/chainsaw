use crate::domain::TaskState;
use crate::domain::test_helpers::{event, format_event};

mod new {
  use super::*;

  #[test]
  fn should_work() {
    let event = event(3, TaskState::Aborted, Some("implementer stalled")).unwrap();

    assert_eq!(
      format_event(&event),
      r#"id: 3
state: aborted
reason: "implementer stalled"
created_at: 2023-11-14T22:13:23Z"#
    );
  }

  #[test]
  fn should_accept_an_event_without_a_reason() {
    let event = event(1, TaskState::Drafted, None).unwrap();

    assert_eq!(
      format_event(&event),
      r#"id: 1
state: drafted
reason: none
created_at: 2023-11-14T22:13:21Z"#
    );
  }

  #[test]
  fn should_fail_when_the_id_is_not_positive() {
    for id in [i64::MIN, -1, 0] {
      let error = event(id, TaskState::Drafted, None).unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_reason_is_blank() {
    for reason in ["", " ", "\n\t"] {
      let error = event(3, TaskState::Aborted, Some(reason)).unwrap_err();
      assert_eq!(error.to_string(), "reason cannot be blank");
    }
  }
}
