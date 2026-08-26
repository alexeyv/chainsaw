use anyhow::Result;
use chrono::{DateTime, Utc};
use strum::IntoEnumIterator;

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
    (TaskState::CommittedUnverified, "committed_unverified"),
    (TaskState::Accepted, "accepted"),
    (TaskState::Aborted, "aborted"),
  ] {
    assert_eq!(state.to_string(), state.as_str());
    assert_eq!(state.to_string(), name);
  }
}

#[test]
fn the_lifecycle_only_moves_forward_and_ends_at_a_terminal_state() {
  let allowed = [
    (TaskState::Drafted, TaskState::Dispatched),
    (TaskState::Drafted, TaskState::InFlight),
    (TaskState::Drafted, TaskState::CommittedUnverified),
    (TaskState::Drafted, TaskState::Accepted),
    (TaskState::Drafted, TaskState::Aborted),
    (TaskState::Dispatched, TaskState::InFlight),
    (TaskState::Dispatched, TaskState::CommittedUnverified),
    (TaskState::Dispatched, TaskState::Accepted),
    (TaskState::Dispatched, TaskState::Aborted),
    (TaskState::InFlight, TaskState::CommittedUnverified),
    (TaskState::InFlight, TaskState::Accepted),
    (TaskState::InFlight, TaskState::Aborted),
    (TaskState::CommittedUnverified, TaskState::Accepted),
    (TaskState::CommittedUnverified, TaskState::Aborted),
  ];

  for current in TaskState::iter() {
    for next in TaskState::iter() {
      assert_eq!(
        current.can_transition_to(next),
        allowed.contains(&(current, next)),
        "unexpected transition {current} -> {next}"
      );
    }
  }
}

#[test]
fn every_nonterminal_state_can_abort_and_no_state_can_leave_a_terminal_one() {
  for state in TaskState::iter() {
    assert_eq!(
      state.can_transition_to(TaskState::Aborted),
      !state.is_terminal(),
      "abort reachability wrong for {state}"
    );
    assert!(
      !state.can_transition_to(state),
      "{state} advances to itself"
    );
  }

  assert!(TaskState::Accepted.is_terminal());
  assert!(TaskState::Aborted.is_terminal());
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
) -> Result<Task> {
  let states = match state {
    TaskState::Drafted => vec![TaskState::Drafted],
    TaskState::Dispatched => vec![TaskState::Drafted, TaskState::Dispatched],
    TaskState::InFlight => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
    ],
    TaskState::CommittedUnverified => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::CommittedUnverified,
    ],
    TaskState::Accepted => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::CommittedUnverified,
      TaskState::Accepted,
    ],
    TaskState::Aborted => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::Aborted,
    ],
  };
  let last = states.len() - 1;
  Task::new(
    id,
    text.to_owned(),
    predicted_files,
    20,
    session_id,
    commit_sha.map(str::to_owned),
    1_700_000_000.0,
    None,
    100,
    Some("abc123".to_owned()),
    file_list.map(|files| files.into_iter().map(str::to_owned).collect()),
    is_session_reuse,
    context_size_start,
    states
      .into_iter()
      .enumerate()
      .map(|(index, state)| {
        TaskEvent::new(
          index as i64 + 1,
          state,
          (index == last)
            .then_some(reason)
            .flatten()
            .map(str::to_owned),
          timestamp(1_700_000_000 + index as i64),
        )
        .unwrap()
      })
      .collect(),
  )
}

fn drafted_task_with_events(events: Vec<TaskEvent>) -> Result<Task> {
  Task::new(
    1,
    "task".to_owned(),
    0,
    0,
    Some(7),
    None,
    1_700_000_000.0,
    None,
    0,
    None,
    None,
    false,
    None,
    events,
  )
}

#[test]
fn exposes_every_field_without_mutators() {
  let events = vec![
    TaskEvent::new(1, TaskState::Drafted, None, timestamp(1_699_999_900)).unwrap(),
    TaskEvent::new(2, TaskState::Dispatched, None, timestamp(1_699_999_950)).unwrap(),
    TaskEvent::new(
      3,
      TaskState::InFlight,
      Some("retrying".to_owned()),
      timestamp(1_700_000_000),
    )
    .unwrap(),
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
    100,
    Some("abc123".to_owned()),
    Some(vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()]),
    true,
    Some(40),
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
    TaskState::CommittedUnverified,
    Some(2),
    None,
    None,
    None,
    false,
    None,
  )
  .unwrap_err();

  assert_eq!(
    error.to_string(),
    "CommittedUnverified task requires a commit"
  );
}

#[test]
fn a_task_aborted_before_it_committed_needs_no_commit() -> Result<()> {
  let task = task_with(
    1,
    "task",
    0,
    TaskState::Aborted,
    Some(2),
    None,
    Some("implementer stalled"),
    None,
    false,
    None,
  )?;

  assert_eq!(task.state(), TaskState::Aborted);
  assert_eq!(task.commit_sha(), None);
  assert_eq!(task.reason(), Some("implementer stalled"));
  Ok(())
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
    0,
    None,
    None,
    false,
    None,
    vec![TaskEvent::new(1, TaskState::Drafted, None, timestamp(1)).unwrap()],
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
  )
  .unwrap_err();

  assert_eq!(
    error.to_string(),
    "session reuse requires an assigned session"
  );
}

#[test]
fn requires_at_least_one_event() {
  let error = drafted_task_with_events(Vec::new()).unwrap_err();

  assert_eq!(error.to_string(), "a task requires at least one event");
}

#[test]
fn lifecycle_starts_drafted() {
  let error = drafted_task_with_events(vec![
    TaskEvent::new(1, TaskState::Dispatched, None, timestamp(1)).unwrap(),
  ])
  .unwrap_err();

  assert_eq!(error.to_string(), "a task's first event must be drafted");
}

#[test]
fn event_ids_are_unique_within_a_task() {
  let error = drafted_task_with_events(vec![
    TaskEvent::new(1, TaskState::Drafted, None, timestamp(1)).unwrap(),
    TaskEvent::new(1, TaskState::Drafted, None, timestamp(2)).unwrap(),
  ])
  .unwrap_err();

  assert_eq!(error.to_string(), "task events contain duplicate id 1");
}

#[test]
fn backward_clock_steps_do_not_change_event_order() -> Result<()> {
  let task = drafted_task_with_events(vec![
    TaskEvent::new(1, TaskState::Drafted, None, timestamp(2)).unwrap(),
    TaskEvent::new(2, TaskState::Dispatched, None, timestamp(1)).unwrap(),
  ])?;

  assert_eq!(task.events()[0].id(), 1);
  assert_eq!(task.events()[1].id(), 2);
  assert!(task.events()[1].created_at() < task.events()[0].created_at());
  Ok(())
}

#[test]
fn events_must_be_in_increasing_identity_order() {
  let error = drafted_task_with_events(vec![
    TaskEvent::new(2, TaskState::Drafted, None, timestamp(1)).unwrap(),
    TaskEvent::new(1, TaskState::Dispatched, None, timestamp(2)).unwrap(),
  ])
  .unwrap_err();

  assert_eq!(
    error.to_string(),
    "task events are out of identity order: 1 follows 2"
  );
}

#[test]
fn event_history_rejects_a_backward_transition() {
  let error = drafted_task_with_events(vec![
    TaskEvent::new(1, TaskState::Drafted, None, timestamp(1)).unwrap(),
    TaskEvent::new(2, TaskState::InFlight, None, timestamp(2)).unwrap(),
    TaskEvent::new(3, TaskState::Dispatched, None, timestamp(3)).unwrap(),
  ])
  .unwrap_err();

  assert_eq!(
    error.to_string(),
    "task cannot transition from in_flight to dispatched"
  );
}

#[test]
fn event_history_rejects_anything_after_a_terminal_state() {
  let error = drafted_task_with_events(vec![
    TaskEvent::new(1, TaskState::Drafted, None, timestamp(1)).unwrap(),
    TaskEvent::new(2, TaskState::Aborted, None, timestamp(2)).unwrap(),
    TaskEvent::new(3, TaskState::Accepted, None, timestamp(3)).unwrap(),
  ])
  .unwrap_err();

  assert_eq!(
    error.to_string(),
    "task cannot transition from aborted to accepted"
  );
}

#[test]
fn a_task_may_abort_straight_from_drafted() -> Result<()> {
  let task = drafted_task_with_events(vec![
    TaskEvent::new(1, TaskState::Drafted, None, timestamp(1)).unwrap(),
    TaskEvent::new(
      2,
      TaskState::Aborted,
      Some("withdrawn".to_owned()),
      timestamp(2),
    )
    .unwrap(),
  ])?;

  assert_eq!(task.state(), TaskState::Aborted);
  assert_eq!(task.reason(), Some("withdrawn"));
  Ok(())
}
