use chrono::Duration;

use crate::domain::HumanWait;
use crate::domain::test_helpers::{ended_wait, format_human_wait, open_wait, timestamp};

mod new {
  use super::*;

  #[test]
  fn should_work() {
    let wait = ended_wait(3).unwrap();

    assert_eq!(
      format_human_wait(&wait),
      r#"id: 3
started: 2023-11-14T22:13:20Z
ended: 2023-11-14T22:18:20Z
is_open: false"#
    );
  }

  #[test]
  fn should_accept_an_open_wait() {
    let wait = open_wait(3).unwrap();

    assert_eq!(
      format_human_wait(&wait),
      r#"id: 3
started: 2023-11-14T22:13:20Z
ended: none
is_open: true"#
    );
  }

  #[test]
  fn should_fail_when_the_id_is_not_positive() {
    for id in [i64::MIN, -1, 0] {
      let error = HumanWait::new(id, timestamp(1_700_000_000), None).unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_end_precedes_the_start() {
    let error =
      HumanWait::new(3, timestamp(1_700_000_000), Some(timestamp(1_699_999_999))).unwrap_err();

    assert_eq!(error.to_string(), "ended cannot precede started");
  }
}

mod duration {
  use super::*;

  #[test]
  fn should_work() {
    let wait = ended_wait(3).unwrap();

    assert_eq!(
      wait.duration(timestamp(1_700_009_999)),
      Duration::seconds(300)
    );
  }

  #[test]
  fn should_run_to_now_while_the_wait_is_open() {
    let wait = open_wait(3).unwrap();

    assert_eq!(
      wait.duration(timestamp(1_700_000_125)),
      Duration::seconds(125)
    );
  }

  #[test]
  fn should_read_as_nothing_when_the_clock_is_behind_the_start() {
    let wait = open_wait(3).unwrap();

    assert_eq!(wait.duration(timestamp(1_699_999_000)), Duration::zero());
  }
}
