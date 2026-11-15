use crate::domain::test_helpers::{
  RunSpec, build_run, format_run, fresh_run, polled_run, timestamp,
};

mod new {
  use super::*;

  #[test]
  fn should_work() {
    let run = build_run(polled_run()).unwrap();
    assert_eq!(
      format_run(&run),
      r#"daemon_seen_at: 2023-11-14T22:23:20Z
stop_requested_at: none
state_read_at: 2023-11-14T22:18:20Z
is_stopping: false
observation_cursor: 4
daemon_pid: 4321"#
    );
  }

  #[test]
  fn should_fail_when_the_observation_cursor_is_negative() {
    let error = build_run(RunSpec {
      observation_cursor: -1,
      ..fresh_run()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "observation_cursor cannot be negative");
  }
}

mod seconds_since_daemon_seen {
  use super::*;

  #[test]
  fn should_work() {
    let run = build_run(polled_run()).unwrap();
    assert_eq!(
      run.seconds_since_daemon_seen(timestamp(1_700_000_725)),
      Some(125)
    );
  }

  #[test]
  fn should_read_as_just_now_when_the_clock_is_behind_the_poll() {
    let run = build_run(polled_run()).unwrap();
    assert_eq!(
      run.seconds_since_daemon_seen(timestamp(1_700_000_599)),
      Some(0)
    );
  }

  #[test]
  fn should_be_unknown_when_no_daemon_has_polled() {
    let run = build_run(fresh_run()).unwrap();
    assert_eq!(
      run.seconds_since_daemon_seen(timestamp(1_700_000_725)),
      None
    );
  }
}

mod seconds_since_state_read {
  use super::*;

  #[test]
  fn should_work() {
    let run = build_run(polled_run()).unwrap();
    assert_eq!(
      run.seconds_since_state_read(timestamp(1_700_000_425)),
      Some(125)
    );
  }

  #[test]
  fn should_read_as_just_now_when_the_clock_is_behind_the_read() {
    let run = build_run(polled_run()).unwrap();
    assert_eq!(
      run.seconds_since_state_read(timestamp(1_700_000_299)),
      Some(0)
    );
  }

  #[test]
  fn should_be_unknown_when_state_has_never_been_read() {
    let run = build_run(fresh_run()).unwrap();
    assert_eq!(run.seconds_since_state_read(timestamp(1_700_000_425)), None);
  }
}
