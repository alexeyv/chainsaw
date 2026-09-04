use strum::IntoEnumIterator;

use super::TaskState;
use crate::domain::TaskEvent;
use crate::domain::test_helpers::{TaskSpec, build, drafted_task, event, format_task, task_in};

mod can_transition_to {
  use super::*;

  #[test]
  fn should_work() {
    assert!(TaskState::Drafted.can_transition_to(TaskState::Dispatched));
    assert!(TaskState::Dispatched.can_transition_to(TaskState::InFlight));
    assert!(TaskState::InFlight.can_transition_to(TaskState::CommittedUnverified));
    assert!(TaskState::CommittedUnverified.can_transition_to(TaskState::Accepted));
  }

  #[test]
  fn should_allow_skipping_forward_over_intermediate_states() {
    assert!(TaskState::Drafted.can_transition_to(TaskState::Accepted));
    assert!(TaskState::Dispatched.can_transition_to(TaskState::CommittedUnverified));
  }

  #[test]
  fn should_allow_aborting_from_every_state_that_is_not_terminal() {
    for state in TaskState::iter() {
      assert_eq!(
        state.can_transition_to(TaskState::Aborted),
        !state.is_terminal(),
        "abort reachability wrong for {state}"
      );
    }
  }

  #[test]
  fn should_refuse_to_stay_in_or_move_backward_from_any_state() {
    for current in TaskState::iter() {
      for next in TaskState::iter().filter(|next| *next <= current) {
        assert!(
          !current.can_transition_to(next),
          "{current} -> {next} was allowed"
        );
      }
    }
  }

  #[test]
  fn should_refuse_to_leave_a_terminal_state() {
    for current in [TaskState::Accepted, TaskState::Aborted] {
      for next in TaskState::iter() {
        assert!(
          !current.can_transition_to(next),
          "{current} -> {next} was allowed"
        );
      }
    }
  }
}

mod is_terminal {
  use super::*;

  #[test]
  fn should_work() {
    assert!(TaskState::Accepted.is_terminal());
    assert!(TaskState::Aborted.is_terminal());
  }

  #[test]
  fn should_be_false_for_every_state_a_task_can_leave() {
    for state in [
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::CommittedUnverified,
    ] {
      assert!(!state.is_terminal(), "{state} is terminal");
    }
  }
}

mod try_from {
  use super::*;

  #[test]
  fn should_work() {
    for (state, name) in [
      (TaskState::Drafted, "drafted"),
      (TaskState::Dispatched, "dispatched"),
      (TaskState::InFlight, "in_flight"),
      (TaskState::CommittedUnverified, "committed_unverified"),
      (TaskState::Accepted, "accepted"),
      (TaskState::Aborted, "aborted"),
    ] {
      assert_eq!(state.as_str(), name);
      assert_eq!(state.to_string(), name);
      assert_eq!(TaskState::try_from(name).unwrap(), state);
    }
  }

  #[test]
  fn should_fail_when_the_name_is_unknown() {
    for name in ["", "Drafted", "inflight", "accepted "] {
      let error = TaskState::try_from(name).unwrap_err();
      assert_eq!(error.to_string(), format!("unknown task state {name:?}"));
    }
  }
}

mod new {
  use super::*;

  #[test]
  fn should_work() {
    let task = build(drafted_task()).unwrap();

    assert_eq!(
      format_task(&task),
      r#"id: 3
text: "implement the task"
predicted_files: 2
predicted_lines: 20
state: drafted
session_id: none
commit_sha: none
created_at: 2023-11-14T22:13:20Z
retry_of_task_id: none
reason: none
log_offset: 0
base_head: none
predicted_file_list: none
context_size_start: none
events:
  1 drafted none"#
    );
  }

  #[test]
  fn should_expose_every_field_of_an_accepted_task() {
    let task = build(TaskSpec {
      retry_of_task_id: Some(2),
      predicted_file_list: Some(vec!["src/a.rs", "src/b.rs"]),
      ..task_in(TaskState::Accepted, Some("gate passed"))
    })
    .unwrap();

    assert_eq!(
      format_task(&task),
      r#"id: 3
text: "implement the task"
predicted_files: 2
predicted_lines: 20
state: accepted
session_id: 7
commit_sha: "abc123"
created_at: 2023-11-14T22:13:20Z
retry_of_task_id: 2
reason: "gate passed"
log_offset: 100
base_head: "base123"
predicted_file_list: ["src/a.rs", "src/b.rs"]
context_size_start: 900
events:
  1 drafted none
  2 dispatched none
  3 in_flight none
  4 committed_unverified none
  5 accepted "gate passed""#
    );
  }

  #[test]
  fn should_accept_a_task_aborted_before_it_committed() {
    let task = build(TaskSpec {
      commit_sha: None,
      ..task_in(TaskState::Aborted, Some("implementer stalled"))
    })
    .unwrap();

    assert_eq!(task.state(), TaskState::Aborted);
    assert_eq!(task.reason(), Some("implementer stalled"));
    assert_eq!(task.commit_sha(), None);
  }

  #[test]
  fn should_accept_a_task_aborted_straight_from_drafted() {
    let task = build(TaskSpec {
      events: vec![
        event(1, TaskState::Drafted, None).unwrap(),
        event(2, TaskState::Aborted, Some("withdrawn")).unwrap(),
      ],
      ..drafted_task()
    })
    .unwrap();

    assert_eq!(task.state(), TaskState::Aborted);
    assert_eq!(task.reason(), Some("withdrawn"));
  }

  #[test]
  fn should_keep_events_in_identity_order_across_a_backward_clock_step() {
    let earlier = event(1, TaskState::Drafted, None).unwrap();
    let later = TaskEvent::new(
      2,
      TaskState::Aborted,
      Some("withdrawn".to_owned()),
      earlier.created_at() - chrono::Duration::seconds(10),
    )
    .unwrap();

    let task = build(TaskSpec {
      events: vec![earlier, later],
      ..drafted_task()
    })
    .unwrap();

    assert_eq!(task.state(), TaskState::Aborted);
    assert!(task.events()[1].created_at() < task.events()[0].created_at());
  }

  #[test]
  fn should_fail_when_the_id_is_not_positive() {
    for id in [i64::MIN, -1, 0] {
      let error = build(TaskSpec {
        id,
        ..drafted_task()
      })
      .unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_text_is_blank() {
    for text in ["", " ", "\n\t"] {
      let error = build(TaskSpec {
        text,
        ..drafted_task()
      })
      .unwrap_err();
      assert_eq!(error.to_string(), "text cannot be blank");
    }
  }

  #[test]
  fn should_fail_when_predicted_files_is_negative() {
    let error = build(TaskSpec {
      predicted_files: -1,
      ..drafted_task()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "predicted_files cannot be negative");
  }

  #[test]
  fn should_fail_when_predicted_lines_is_negative() {
    let error = build(TaskSpec {
      predicted_lines: -1,
      ..drafted_task()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "predicted_lines cannot be negative");
  }

  #[test]
  fn should_fail_when_the_session_id_is_not_positive() {
    for session_id in [i64::MIN, -1, 0] {
      let error = build(TaskSpec {
        session_id: Some(session_id),
        ..drafted_task()
      })
      .unwrap_err();
      assert_eq!(error.to_string(), "session_id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_commit_sha_is_blank() {
    let error = build(TaskSpec {
      commit_sha: Some(" "),
      ..drafted_task()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "commit_sha cannot be blank");
  }

  #[test]
  fn should_fail_when_the_retried_task_id_is_not_positive() {
    for retry_of_task_id in [i64::MIN, -1, 0] {
      let error = build(TaskSpec {
        retry_of_task_id: Some(retry_of_task_id),
        ..drafted_task()
      })
      .unwrap_err();
      assert_eq!(error.to_string(), "retry_of_task_id must be positive");
    }
  }

  #[test]
  fn should_fail_when_a_task_retries_itself() {
    let error = build(TaskSpec {
      id: 3,
      retry_of_task_id: Some(3),
      ..drafted_task()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "a task cannot retry itself");
  }

  #[test]
  fn should_fail_when_the_log_offset_is_negative() {
    let error = build(TaskSpec {
      log_offset: -1,
      ..drafted_task()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "log_offset cannot be negative");
  }

  #[test]
  fn should_fail_when_the_base_head_is_blank() {
    let error = build(TaskSpec {
      base_head: Some(""),
      ..drafted_task()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "base_head cannot be blank");
  }

  #[test]
  fn should_fail_when_the_starting_context_size_is_negative() {
    let error = build(TaskSpec {
      context_size_start: Some(-1),
      ..drafted_task()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "context_size_start cannot be negative");
  }

  #[test]
  fn should_fail_when_a_state_past_drafted_has_no_session() {
    for state in [
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::CommittedUnverified,
      TaskState::Accepted,
    ] {
      let error = build(TaskSpec {
        session_id: None,
        ..task_in(state, None)
      })
      .unwrap_err();
      assert_eq!(
        error.to_string(),
        format!("{state:?} task requires a session")
      );
    }
  }

  #[test]
  fn should_fail_when_a_committed_state_has_no_commit() {
    for state in [TaskState::CommittedUnverified, TaskState::Accepted] {
      let error = build(TaskSpec {
        commit_sha: None,
        ..task_in(state, None)
      })
      .unwrap_err();
      assert_eq!(
        error.to_string(),
        format!("{state:?} task requires a commit")
      );
    }
  }
}

mod validate_predicted_files {
  use super::*;

  #[test]
  fn should_work() {
    let task = build(TaskSpec {
      predicted_files: 2,
      predicted_file_list: Some(vec!["src/a.rs", "src/b.rs"]),
      ..drafted_task()
    })
    .unwrap();
    assert_eq!(
      task.predicted_file_list(),
      Some(["src/a.rs".to_owned(), "src/b.rs".to_owned()].as_slice())
    );
  }

  #[test]
  fn should_accept_any_count_when_there_is_no_list() {
    let task = build(TaskSpec {
      predicted_files: 9,
      predicted_file_list: None,
      ..drafted_task()
    })
    .unwrap();
    assert_eq!(task.predicted_files(), 9);
  }

  #[test]
  fn should_accept_an_empty_list_with_a_zero_count() {
    let task = build(TaskSpec {
      predicted_files: 0,
      predicted_file_list: Some(vec![]),
      ..drafted_task()
    })
    .unwrap();
    assert_eq!(task.predicted_file_list(), Some([].as_slice()));
  }

  #[test]
  fn should_fail_when_the_count_does_not_match_the_list() {
    let error = build(TaskSpec {
      predicted_files: 1,
      predicted_file_list: Some(vec!["src/a.rs", "src/b.rs"]),
      ..drafted_task()
    })
    .unwrap_err();
    assert_eq!(
      error.to_string(),
      "predicted file count 1 does not match 2 listed files"
    );
  }

  #[test]
  fn should_fail_when_a_listed_file_is_blank() {
    let error = build(TaskSpec {
      predicted_files: 2,
      predicted_file_list: Some(vec!["src/a.rs", " "]),
      ..drafted_task()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "predicted_file_list cannot be blank");
  }

  #[test]
  fn should_fail_when_a_file_is_listed_twice() {
    let error = build(TaskSpec {
      predicted_files: 2,
      predicted_file_list: Some(vec!["src/a.rs", "src/a.rs"]),
      ..drafted_task()
    })
    .unwrap_err();
    assert_eq!(
      error.to_string(),
      "predicted file list contains duplicate \"src/a.rs\""
    );
  }
}

mod validate_events {
  use super::*;

  fn with_events(events: Vec<TaskEvent>) -> anyhow::Error {
    build(TaskSpec {
      events,
      ..task_in(TaskState::Accepted, None)
    })
    .unwrap_err()
  }

  #[test]
  fn should_work() {
    for state in TaskState::iter() {
      let task = build(task_in(state, None)).unwrap();
      assert_eq!(task.state(), state);
    }
  }

  #[test]
  fn should_fail_when_there_are_no_events() {
    let error = with_events(vec![]);
    assert_eq!(error.to_string(), "a task requires at least one event");
  }

  #[test]
  fn should_fail_when_the_first_event_is_not_drafted() {
    let error = with_events(vec![event(1, TaskState::Dispatched, None).unwrap()]);
    assert_eq!(error.to_string(), "a task's first event must be drafted");
  }

  #[test]
  fn should_fail_when_two_events_share_an_id() {
    let error = with_events(vec![
      event(1, TaskState::Drafted, None).unwrap(),
      event(1, TaskState::Dispatched, None).unwrap(),
    ]);
    assert_eq!(error.to_string(), "task events contain duplicate id 1");
  }

  #[test]
  fn should_fail_when_event_ids_do_not_increase() {
    let error = with_events(vec![
      event(2, TaskState::Drafted, None).unwrap(),
      event(1, TaskState::Dispatched, None).unwrap(),
    ]);
    assert_eq!(
      error.to_string(),
      "task events are out of identity order: 1 follows 2"
    );
  }

  #[test]
  fn should_fail_when_the_history_moves_backward() {
    let error = with_events(vec![
      event(1, TaskState::Drafted, None).unwrap(),
      event(2, TaskState::InFlight, None).unwrap(),
      event(3, TaskState::Dispatched, None).unwrap(),
    ]);
    assert_eq!(
      error.to_string(),
      "task cannot transition from in_flight to dispatched"
    );
  }

  #[test]
  fn should_fail_when_the_history_continues_past_a_terminal_state() {
    let error = with_events(vec![
      event(1, TaskState::Drafted, None).unwrap(),
      event(2, TaskState::Aborted, Some("stalled")).unwrap(),
      event(3, TaskState::Accepted, None).unwrap(),
    ]);
    assert_eq!(
      error.to_string(),
      "task cannot transition from aborted to accepted"
    );
  }
}
