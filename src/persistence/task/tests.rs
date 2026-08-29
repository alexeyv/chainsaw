use anyhow::Result;
use chrono::Utc;
use rusqlite::Transaction;

use super::{
  abort, accept, advance, all, create, dispatch, get, predecessor, record_commit, take_flight,
  tasks_for_session,
};
use crate::domain::test_helpers::{format_task, format_tasks, format_time, within};
use crate::domain::{Task, TaskState};
use crate::persistence::test_fixture::{database, row_count, session_row, task_row};

/// A drafted task with no estimate, file list, or predecessor.
fn draft(transaction: &Transaction<'_>, text: &str) -> Result<Task> {
  create(transaction, text, 0, 10, None, None)
}

/// A task dispatched to session 7, which the caller must have created.
fn dispatched(transaction: &Transaction<'_>, text: &str) -> Result<Task> {
  let task = draft(transaction, text)?;
  dispatch(transaction, task.id(), 7, 0, false, None)
}

fn states_of(task: &Task) -> Vec<TaskState> {
  task.events().iter().map(|event| event.state()).collect()
}

mod create {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let files = vec!["src/domain/task.rs".to_owned(), "src/store.rs".to_owned()];

    let before = Utc::now();
    let transaction = db.transaction()?;
    let task = create(
      &transaction,
      "materialize tasks through persistence",
      2,
      80,
      None,
      Some(files.clone()),
    )?;
    transaction.commit()?;
    let after = Utc::now();

    let stored = db.query_row(
      "
        select text, predicted_files, predicted_lines, created_at,
               retry_of_task_id, predicted_file_list
        from tasks where id=?
        ",
      [task.id()],
      |row| {
        Ok((
          row.get::<_, String>(0)?,
          row.get::<_, i64>(1)?,
          row.get::<_, i64>(2)?,
          row.get::<_, i64>(3)?,
          row.get::<_, Option<i64>>(4)?,
          row.get::<_, Option<String>>(5)?,
        ))
      },
    )?;
    let stored_event = db.query_row(
      "select task_id, state from task_events where id=?",
      [task.events()[0].id()],
      |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    )?;

    assert!(within(task.created_at(), before, after));
    assert_eq!(
      format_task(&task),
      format!(
        r#"id: 1
text: "materialize tasks through persistence"
predicted_files: 2
predicted_lines: 80
state: drafted
session_id: none
commit_sha: none
created_at: {}
retry_of_task_id: none
reason: none
log_offset: 0
base_head: none
predicted_file_list: ["src/domain/task.rs", "src/store.rs"]
is_session_reuse: false
context_size_start: none
events:
  1 drafted none"#,
        format_time(task.created_at())
      )
    );
    assert_eq!(
      stored,
      (
        "materialize tasks through persistence".to_owned(),
        2,
        80,
        task.created_at().timestamp_millis(),
        None,
        Some("src/domain/task.rs,src/store.rs".to_owned()),
      )
    );
    assert_eq!(stored_event, (1, "drafted".to_owned()));
    Ok(())
  }

  #[test]
  fn should_assign_increasing_ids() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let first = draft(&transaction, "first")?;
    let second = draft(&transaction, "second")?;
    transaction.commit()?;

    assert_eq!((first.id(), second.id()), (1, 2));
    Ok(())
  }

  #[test]
  fn should_record_which_task_is_being_retried() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let first = draft(&transaction, "first attempt")?;
    let retry = create(
      &transaction,
      "second attempt",
      0,
      10,
      Some(first.id()),
      None,
    )?;
    transaction.commit()?;

    assert_eq!(retry.retry_of_task_id(), Some(first.id()));
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    draft(&transaction, "a task")?;
    transaction.rollback()?;

    assert_eq!(row_count(&db, "tasks")?, 0);
    assert_eq!(row_count(&db, "task_events")?, 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_text_is_blank() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = draft(&transaction, " ").unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "text cannot be blank");
    Ok(())
  }

  #[test]
  fn should_fail_when_the_file_count_does_not_match_the_list() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(
      &transaction,
      "a task",
      1,
      10,
      None,
      Some(vec!["a.rs".to_owned(), "b.rs".to_owned()]),
    )
    .unwrap_err();
    transaction.rollback()?;

    assert_eq!(
      error.to_string(),
      "predicted file count 1 does not match 2 listed files"
    );
    Ok(())
  }

  #[test]
  fn should_store_no_list_when_the_predicted_file_list_is_empty() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let task = create(&transaction, "a task", 0, 10, None, Some(Vec::new()))?;
    transaction.commit()?;

    assert_eq!(task.predicted_file_list(), None);
    assert_eq!(
      db.query_row(
        "select predicted_file_list from tasks where id=?",
        [task.id()],
        |row| row.get::<_, Option<String>>(0),
      )?,
      None
    );
    Ok(())
  }

  #[test]
  fn should_fail_when_a_predicted_file_name_contains_the_separator() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(
      &transaction,
      "a task",
      1,
      10,
      None,
      Some(vec!["a,b.rs".to_owned()]),
    )
    .unwrap_err();
    transaction.rollback()?;

    assert_eq!(
      error.to_string(),
      "predicted file name \"a,b.rs\" contains ','"
    );
    Ok(())
  }

  #[test]
  fn should_fail_when_the_retried_task_does_not_exist() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(&transaction, "a retry", 0, 10, Some(7), None).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    Ok(())
  }
}

mod get {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "inspect snapshots")?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    let loaded = get(&transaction, task.id())?.expect("stored task");
    transaction.commit()?;

    assert_eq!(format_task(&loaded), format_task(&task));
    Ok(())
  }

  #[test]
  fn should_return_none_when_the_task_does_not_exist() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    assert_eq!(get(&transaction, 1)?, None);
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_keep_events_in_identity_order_across_a_backward_clock_step() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;

    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "inspect snapshots")?;
    let dispatch_event = task.events().last().expect("dispatch event");
    transaction.execute(
      "update task_events set created_at=? where id=?",
      [
        task.events()[0].created_at().timestamp_millis() - 1_000,
        dispatch_event.id(),
      ],
    )?;
    let loaded = get(&transaction, task.id())?.expect("stored task");
    transaction.commit()?;

    assert_eq!(
      states_of(&loaded),
      vec![TaskState::Drafted, TaskState::Dispatched]
    );
    assert!(loaded.events()[1].created_at() < loaded.events()[0].created_at());
    Ok(())
  }

  #[test]
  fn should_read_a_missing_log_offset_as_zero() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let task = draft(&transaction, "no offset yet")?;
    transaction.execute("update tasks set log_offset=null where id=?", [task.id()])?;
    let loaded = get(&transaction, task.id())?.expect("stored task");
    transaction.commit()?;

    assert_eq!(loaded.log_offset(), 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_a_stored_event_state_is_unknown() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let task = draft(&transaction, "corrupt")?;
    transaction.execute(
      "update task_events set state='parked' where task_id=?",
      [task.id()],
    )?;

    let error = get(&transaction, task.id()).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "unknown task state \"parked\"");
    Ok(())
  }

  #[test]
  fn should_fail_when_the_stored_history_is_invalid() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let task = draft(&transaction, "corrupt")?;
    transaction.execute("delete from task_events where task_id=?", [task.id()])?;

    let error = get(&transaction, task.id()).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "a task requires at least one event");
    Ok(())
  }
}

mod all {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let first = dispatched(&transaction, "first")?;
    let second = draft(&transaction, "second")?;

    assert_eq!(
      format_tasks(&all(&transaction)?),
      format_tasks(&[first, second])
    );
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_return_nothing_when_there_are_no_tasks() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    assert_eq!(all(&transaction)?, Vec::new());
    transaction.commit()?;
    Ok(())
  }
}

mod tasks_for_session {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    session_row(&db, 8)?;
    let transaction = db.transaction()?;
    let first = dispatched(&transaction, "first target task")?;
    let other = draft(&transaction, "other session task")?;
    dispatch(&transaction, other.id(), 8, 0, false, None)?;
    let second = dispatched(&transaction, "second target task")?;

    assert_eq!(
      format_tasks(&tasks_for_session(&transaction, 7)?),
      format_tasks(&[first, second])
    );
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_return_nothing_when_the_session_has_no_tasks() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    draft(&transaction, "unassigned")?;

    assert_eq!(tasks_for_session(&transaction, 7)?, Vec::new());
    transaction.commit()?;
    Ok(())
  }
}

mod predecessor {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    draft(&transaction, "first")?;
    let second = dispatched(&transaction, "second")?;
    let third = draft(&transaction, "third")?;

    let found = predecessor(&transaction, third.id())?.expect("predecessor");
    assert_eq!(format_task(&found), format_task(&second));
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_skip_gaps_in_identity() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let first = draft(&transaction, "first")?;
    task_row(&transaction, 50)?;
    transaction.execute("delete from tasks where id=50", [])?;

    let found = predecessor(&transaction, i64::MAX)?.expect("predecessor");
    assert_eq!(format_task(&found), format_task(&first));
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_return_none_for_the_first_task() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let first = draft(&transaction, "first")?;

    assert_eq!(predecessor(&transaction, first.id())?, None);
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_return_none_when_there_are_no_tasks() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    assert_eq!(predecessor(&transaction, 1)?, None);
    transaction.commit()?;
    Ok(())
  }
}

mod dispatch {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = draft(&transaction, "dispatch me")?;

    let task = dispatch(&transaction, task.id(), 7, 42, true, Some("reuse"))?;
    transaction.commit()?;

    assert_eq!(task.state(), TaskState::Dispatched);
    assert_eq!(task.session_id(), Some(7));
    assert_eq!(task.log_offset(), 42);
    assert!(task.is_session_reuse());
    assert_eq!(task.reason(), Some("reuse"));
    assert_eq!(
      states_of(&task),
      vec![TaskState::Drafted, TaskState::Dispatched]
    );
    Ok(())
  }

  #[test]
  fn should_record_a_fresh_session_without_a_reason() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = draft(&transaction, "dispatch me")?;

    let task = dispatch(&transaction, task.id(), 7, 0, false, None)?;
    transaction.commit()?;

    assert!(!task.is_session_reuse());
    assert_eq!(task.reason(), None);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_session_does_not_exist() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let task = draft(&transaction, "dispatch me")?;

    let error = dispatch(&transaction, task.id(), 7, 0, false, None).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    Ok(())
  }
}

mod take_flight {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "fly")?;

    let task = take_flight(&transaction, task.id(), 42, "base123", 900)?;
    transaction.commit()?;

    assert_eq!(task.state(), TaskState::InFlight);
    assert_eq!(task.log_offset(), 42);
    assert_eq!(task.base_head(), Some("base123"));
    assert_eq!(task.context_size_start(), Some(900));
    assert_eq!(task.reason(), None);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_has_no_session() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let task = draft(&transaction, "fly")?;

    let error = take_flight(&transaction, task.id(), 42, "base123", 900).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "InFlight task requires a session");
    Ok(())
  }
}

mod record_commit {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "commit")?;
    let task = take_flight(&transaction, task.id(), 42, "base123", 900)?;

    let task = record_commit(&transaction, task.id(), "landed123", Some("hooks ran"))?;
    transaction.commit()?;

    assert_eq!(task.state(), TaskState::CommittedUnverified);
    assert_eq!(task.commit_sha(), Some("landed123"));
    assert_eq!(task.reason(), Some("hooks ran"));
    Ok(())
  }

  #[test]
  fn should_keep_the_first_commit_when_recorded_again() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "commit once")?;
    let first = record_commit(&transaction, task.id(), "first123", None)?;

    let again = record_commit(&transaction, task.id(), "second456", None)?;
    transaction.commit()?;

    assert_eq!(format_task(&again), format_task(&first));
    assert_eq!(again.commit_sha(), Some("first123"));
    Ok(())
  }
}

mod accept {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "accept me")?;
    let task = take_flight(&transaction, task.id(), 42, "base123", 900)?;
    let task = record_commit(&transaction, task.id(), "landed123", None)?;

    let task = accept(&transaction, task.id(), "gate passed")?;
    transaction.commit()?;

    assert_eq!(task.state(), TaskState::Accepted);
    assert_eq!(task.reason(), Some("gate passed"));
    assert_eq!(
      states_of(&task),
      vec![
        TaskState::Drafted,
        TaskState::Dispatched,
        TaskState::InFlight,
        TaskState::CommittedUnverified,
        TaskState::Accepted,
      ]
    );
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_has_no_commit() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "accept me")?;

    let error = accept(&transaction, task.id(), "gate passed").unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "Accepted task requires a commit");
    Ok(())
  }
}

mod abort {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "abort me")?;

    let task = abort(&transaction, task.id(), "implementation stalled")?;
    transaction.commit()?;

    assert_eq!(task.state(), TaskState::Aborted);
    assert_eq!(task.reason(), Some("implementation stalled"));
    Ok(())
  }

  #[test]
  fn should_abort_a_task_that_was_never_dispatched() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let task = draft(&transaction, "abort while drafted")?;

    let task = abort(&transaction, task.id(), "spec withdrawn")?;
    transaction.commit()?;

    assert_eq!(task.state(), TaskState::Aborted);
    assert_eq!(task.reason(), Some("spec withdrawn"));
    Ok(())
  }

  #[test]
  fn should_keep_the_commit_when_aborting_after_a_commit() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "abort after commit")?;
    let task = record_commit(&transaction, task.id(), "landed123", None)?;

    let task = abort(&transaction, task.id(), "reverted, gate never ran")?;
    transaction.commit()?;

    assert_eq!(task.state(), TaskState::Aborted);
    assert_eq!(task.commit_sha(), Some("landed123"));
    assert_eq!(task.reason(), Some("reverted, gate never ran"));
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_is_already_accepted() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "done")?;
    let task = record_commit(&transaction, task.id(), "landed123", None)?;
    let task = accept(&transaction, task.id(), "gate passed")?;

    let error = abort(&transaction, task.id(), "too late").unwrap_err();
    transaction.rollback()?;

    assert_eq!(
      error.to_string(),
      "task 1 cannot advance from accepted to aborted"
    );
    Ok(())
  }
}

mod advance {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = draft(&transaction, "advance me")?;

    let task = advance(
      &transaction,
      task.id(),
      TaskState::Dispatched,
      Some("by hand"),
      |transaction| {
        transaction.execute("update tasks set session_id=7 where id=1", [])?;
        Ok(())
      },
    )?;
    transaction.commit()?;

    assert_eq!(task.state(), TaskState::Dispatched);
    assert_eq!(task.session_id(), Some(7));
    assert_eq!(task.reason(), Some("by hand"));
    Ok(())
  }

  #[test]
  fn should_do_nothing_when_the_task_is_already_in_that_state() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let task = draft(&transaction, "stay drafted")?;

    let same = advance(
      &transaction,
      task.id(),
      TaskState::Drafted,
      Some("ignored"),
      |_| panic!("mutation must not run"),
    )?;
    transaction.commit()?;

    assert_eq!(format_task(&same), format_task(&task));
    assert_eq!(row_count(&db, "task_events")?, 1);
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = draft(&transaction, "remain drafted")?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    dispatch(&transaction, task.id(), 7, 0, false, None)?;
    transaction.rollback()?;

    let transaction = db.transaction()?;
    let unchanged = get(&transaction, task.id())?.expect("drafted task");
    transaction.commit()?;
    assert_eq!(format_task(&unchanged), format_task(&task));
    Ok(())
  }

  #[test]
  fn should_fail_without_mutating_when_the_transition_moves_backward() -> Result<()> {
    let mut db = database();
    session_row(&db, 7)?;
    let transaction = db.transaction()?;
    let task = dispatched(&transaction, "stay aborted")?;
    let task = abort(&transaction, task.id(), "implementation failed")?;

    let error = record_commit(&transaction, task.id(), "unexpected123", None).unwrap_err();
    let unchanged = get(&transaction, task.id())?.expect("aborted task");
    transaction.commit()?;

    assert_eq!(
      error.to_string(),
      "task 1 cannot advance from aborted to committed_unverified"
    );
    assert_eq!(format_task(&unchanged), format_task(&task));
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_does_not_exist() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let error = advance(&transaction, 9, TaskState::Aborted, None, |_| Ok(())).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "task 9 is missing");
    Ok(())
  }

  #[test]
  fn should_fail_when_the_mutation_fails() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let task = draft(&transaction, "unmovable")?;

    let error = advance(
      &transaction,
      task.id(),
      TaskState::Aborted,
      Some("why"),
      |_| anyhow::bail!("mutation refused"),
    )
    .unwrap_err();
    let unchanged = get(&transaction, task.id())?.expect("drafted task");
    transaction.rollback()?;

    assert_eq!(error.to_string(), "mutation refused");
    assert_eq!(format_task(&unchanged), format_task(&task));
    Ok(())
  }

  #[test]
  fn should_fail_when_the_resulting_task_would_be_invalid() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let task = draft(&transaction, "no session")?;

    let error = advance(&transaction, task.id(), TaskState::InFlight, None, |_| {
      Ok(())
    })
    .unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "InFlight task requires a session");
    Ok(())
  }
}
