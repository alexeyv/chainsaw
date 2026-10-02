use crate::domain::test_helpers::{format_observation, observation};

mod new {
  use super::*;

  #[test]
  fn should_work() {
    let observation = observation(3, Some(5), "the gate ran twice").unwrap();

    assert_eq!(
      format_observation(&observation),
      r#"id: 3
task_id: 5
text: "the gate ran twice"
created_at: 2023-11-14T22:13:20Z"#
    );
  }

  #[test]
  fn should_accept_a_run_wide_observation_with_no_task() {
    let observation = observation(3, None, "run wide").unwrap();

    assert_eq!(
      format_observation(&observation),
      r#"id: 3
task_id: none
text: "run wide"
created_at: 2023-11-14T22:13:20Z"#
    );
  }

  #[test]
  fn should_fail_when_the_id_is_not_positive() {
    for id in [i64::MIN, -1, 0] {
      let error = observation(id, Some(5), "text").unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_task_id_is_not_positive() {
    for task_id in [i64::MIN, -1, 0] {
      let error = observation(3, Some(task_id), "text").unwrap_err();
      assert_eq!(error.to_string(), "task_id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_text_is_blank() {
    for text in ["", " ", "\n\t"] {
      let error = observation(3, Some(5), text).unwrap_err();
      assert_eq!(error.to_string(), "text cannot be blank");
    }
  }
}
