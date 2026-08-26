use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::domain::{Task, TaskEvent, TaskState};

struct TaskRow {
  id: i64,
  text: String,
  predicted_files: i64,
  predicted_lines: i64,
  session_id: Option<i64>,
  commit_sha: Option<String>,
  created_at: f64,
  retry_of_task_id: Option<i64>,
  log_offset: i64,
  base_head: Option<String>,
  predicted_file_list: Option<Vec<String>>,
  is_session_reuse: bool,
  context_size_start: Option<i64>,
}

pub fn create(
  transaction: &Transaction<'_>,
  text: &str,
  predicted_files: i64,
  predicted_lines: i64,
  retry_of_task_id: Option<i64>,
  predicted_file_list: Option<Vec<String>>,
) -> Result<Task> {
  let created_at = Utc::now().timestamp_millis() as f64 / 1000.0;
  let stored_file_list = predicted_file_list.as_ref().map(|files| files.join(","));
  let id = transaction.query_row(
    "
      insert into tasks(
        text, predicted_files, predicted_lines, created_at, retry_of_task_id,
        predicted_file_list
      ) values (?1, ?2, ?3, ?4, ?5, ?6)
      returning id
      ",
    params![
      text,
      predicted_files,
      predicted_lines,
      created_at,
      retry_of_task_id,
      stored_file_list,
    ],
    |row| row.get(0),
  )?;
  super::task_event::create(transaction, id, TaskState::Drafted, None)?;
  get(transaction, id)?.with_context(|| format!("created task {id} is missing"))
}

pub fn get(transaction: &Transaction<'_>, id: i64) -> Result<Option<Task>> {
  let row = transaction
    .query_row(
      "
        select id, text, predicted_files, predicted_lines, session_id,
               commit_sha, created_at, retry_of_task_id, log_offset,
               base_head, predicted_file_list, is_session_reuse,
               context_size_start
        from tasks where id=?
        ",
      [id],
      task_row,
    )
    .optional()?;
  row.map(|row| materialize(transaction, row)).transpose()
}

pub fn all(transaction: &Transaction<'_>) -> Result<Vec<Task>> {
  let rows = {
    let mut statement = transaction.prepare(
      "
        select id, text, predicted_files, predicted_lines, session_id,
               commit_sha, created_at, retry_of_task_id, log_offset,
               base_head, predicted_file_list, is_session_reuse,
               context_size_start
        from tasks order by id asc
        ",
    )?;
    statement
      .query_map([], task_row)?
      .collect::<rusqlite::Result<Vec<_>>>()?
  };
  rows
    .into_iter()
    .map(|row| materialize(transaction, row))
    .collect()
}

pub fn tasks_for_session(transaction: &Transaction<'_>, session_id: i64) -> Result<Vec<Task>> {
  let rows = {
    let mut statement = transaction.prepare(
      "
        select id, text, predicted_files, predicted_lines, session_id,
               commit_sha, created_at, retry_of_task_id, log_offset,
               base_head, predicted_file_list, is_session_reuse,
               context_size_start
        from tasks where session_id=? order by id asc
        ",
    )?;
    statement
      .query_map([session_id], task_row)?
      .collect::<rusqlite::Result<Vec<_>>>()?
  };
  rows
    .into_iter()
    .map(|row| materialize(transaction, row))
    .collect()
}

pub fn predecessor(transaction: &Transaction<'_>, id: i64) -> Result<Option<Task>> {
  let row = transaction
    .query_row(
      "
        select id, text, predicted_files, predicted_lines, session_id,
               commit_sha, created_at, retry_of_task_id, log_offset,
               base_head, predicted_file_list, is_session_reuse,
               context_size_start
        from tasks where id < ? order by id desc limit 1
        ",
      [id],
      task_row,
    )
    .optional()?;
  row.map(|row| materialize(transaction, row)).transpose()
}

pub fn dispatch(
  transaction: &Transaction<'_>,
  id: i64,
  session_id: i64,
  reuse: bool,
  reason: Option<&str>,
) -> Result<Task> {
  advance(
    transaction,
    id,
    TaskState::Dispatched,
    reason,
    |transaction| {
      transaction.execute(
        "update tasks set session_id=?, is_session_reuse=? where id=?",
        params![session_id, i64::from(reuse), id],
      )?;
      Ok(())
    },
  )
}

pub fn take_flight(
  transaction: &Transaction<'_>,
  id: i64,
  log_offset: i64,
  base_head: &str,
  context_size_start: i64,
) -> Result<Task> {
  advance(transaction, id, TaskState::InFlight, None, |transaction| {
    transaction.execute(
      "update tasks set log_offset=?, base_head=?, context_size_start=? where id=?",
      params![log_offset, base_head, context_size_start, id],
    )?;
    Ok(())
  })
}

pub fn record_commit(
  transaction: &Transaction<'_>,
  id: i64,
  commit_sha: &str,
  reason: Option<&str>,
) -> Result<Task> {
  advance(
    transaction,
    id,
    TaskState::Committed,
    reason,
    |transaction| {
      transaction.execute(
        "update tasks set commit_sha=? where id=?",
        params![commit_sha, id],
      )?;
      Ok(())
    },
  )
}

pub fn accept(transaction: &Transaction<'_>, id: i64, reason: &str) -> Result<Task> {
  advance(transaction, id, TaskState::Accepted, Some(reason), |_| {
    Ok(())
  })
}

pub fn abort(transaction: &Transaction<'_>, id: i64, reason: &str) -> Result<Task> {
  advance(
    transaction,
    id,
    TaskState::Aborted,
    Some(reason),
    |_| Ok(()),
  )
}

/// Move a task forward to `next`, recording an optional reason for the move.
/// Advancing to the state a task already occupies is a no-op that succeeds, so
/// callers that re-observe the same fact do not have to guard against it.
pub fn advance(
  transaction: &Transaction<'_>,
  id: i64,
  next: TaskState,
  reason: Option<&str>,
  mutate: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<Task> {
  let current = get(transaction, id)?.with_context(|| format!("task {id} is missing"))?;
  if current.state() == next {
    return Ok(current);
  }
  if !current.state().can_transition_to(next) {
    bail!(
      "task {id} cannot advance from {} to {next}",
      current.state()
    );
  }
  mutate(transaction)?;
  super::task_event::create(transaction, id, next, reason)?;
  get(transaction, id)?.with_context(|| format!("task {id} disappeared while becoming {next}"))
}

fn task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
  let predicted_file_list = row
    .get::<_, Option<String>>("predicted_file_list")?
    .map(|files| files.split(',').map(str::to_owned).collect());
  Ok(TaskRow {
    id: row.get("id")?,
    text: row.get("text")?,
    predicted_files: row.get("predicted_files")?,
    predicted_lines: row.get("predicted_lines")?,
    session_id: row.get("session_id")?,
    commit_sha: row.get("commit_sha")?,
    created_at: row.get("created_at")?,
    retry_of_task_id: row.get("retry_of_task_id")?,
    log_offset: row.get::<_, Option<i64>>("log_offset")?.unwrap_or_default(),
    base_head: row.get("base_head")?,
    predicted_file_list,
    is_session_reuse: row.get::<_, i64>("is_session_reuse")? != 0,
    context_size_start: row.get("context_size_start")?,
  })
}

fn materialize(transaction: &Transaction<'_>, row: TaskRow) -> Result<Task> {
  let events = load_events(transaction, row.id)?;
  Task::new(
    row.id,
    row.text,
    row.predicted_files,
    row.predicted_lines,
    row.session_id,
    row.commit_sha,
    row.created_at,
    row.retry_of_task_id,
    row.log_offset,
    row.base_head,
    row.predicted_file_list,
    row.is_session_reuse,
    row.context_size_start,
    events,
  )
}

fn load_events(transaction: &Transaction<'_>, task_id: i64) -> Result<Vec<TaskEvent>> {
  let mut statement = transaction.prepare(
    "
      select id, state, reason, created_at
      from task_events where task_id=? order by id
      ",
  )?;
  let rows = statement.query_map([task_id], |row| {
    Ok((
      row.get::<_, i64>(0)?,
      row.get::<_, String>(1)?,
      row.get::<_, Option<String>>(2)?,
      row.get::<_, i64>(3)?,
    ))
  })?;
  rows
    .map(|row| {
      let (id, state, reason, created_at) = row?;
      let state = TaskState::try_from(state.as_str())?;
      let created_at = DateTime::from_timestamp_millis(created_at)
        .context("task event created_at is outside the supported range")?;
      TaskEvent::new(id, state, reason, created_at)
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use anyhow::Result;
  use chrono::Utc;

  use super::{
    abort, accept, all, create, dispatch, get, predecessor, record_commit, take_flight,
    tasks_for_session,
  };
  use crate::domain::TaskState;
  use crate::persistence::task_event;
  use crate::persistence::test_fixture::database;

  #[test]
  fn creates_a_valid_drafted_task() -> Result<()> {
    let mut db = database();
    let files = vec!["src/domain/task.rs".to_owned(), "src/store.rs".to_owned()];

    let before = Utc::now().timestamp_millis() as f64 / 1000.0;
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
    let after = Utc::now().timestamp_millis() as f64 / 1000.0;

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
          row.get::<_, f64>(3)?,
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

    assert_eq!(task.text(), "materialize tasks through persistence");
    assert_eq!(task.predicted_files(), 2);
    assert_eq!(task.predicted_lines(), 80);
    assert_eq!(task.state(), TaskState::Drafted);
    assert_eq!(task.predicted_file_list(), Some(files.as_slice()));
    assert!(task.created_at() >= before);
    assert!(task.created_at() <= after);
    assert_eq!(task.events().len(), 1);
    assert_eq!(task.events()[0].state(), TaskState::Drafted);
    assert_eq!(
      stored,
      (
        "materialize tasks through persistence".to_owned(),
        2,
        80,
        task.created_at(),
        None,
        Some("src/domain/task.rs,src/store.rs".to_owned()),
      )
    );
    assert_eq!(stored_event, (task.id(), "drafted".to_owned()));
    Ok(())
  }

  #[test]
  fn assigns_autoincrementing_ids() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let first = create(&transaction, "first", 0, 10, None, None)?;
    let second = create(&transaction, "second", 0, 20, None, None)?;
    transaction.commit()?;

    assert_eq!(first.id(), 1);
    assert_eq!(second.id(), 2);
    Ok(())
  }

  #[test]
  fn creates_a_retry_of_an_existing_task() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let first = create(&transaction, "first attempt", 0, 10, None, None)?;
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
  fn leaves_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    create(&transaction, "a task", 0, 10, None, None)?;
    transaction.rollback()?;

    let task_count = db.query_row("select count(*) from tasks", [], |row| row.get::<_, i64>(0))?;
    let event_count = db.query_row("select count(*) from task_events", [], |row| {
      row.get::<_, i64>(0)
    })?;
    assert_eq!(task_count, 0);
    assert_eq!(event_count, 0);
    Ok(())
  }

  #[test]
  fn rejects_invalid_data_before_commit() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(&transaction, " ", 0, 10, None, None).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "text cannot be blank");
    Ok(())
  }

  #[test]
  fn rejects_a_missing_retry_task() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(&transaction, "a retry", 0, 10, Some(7), None).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    Ok(())
  }

  #[test]
  fn reads_a_fully_materialized_task_in_identity_order_across_a_backward_clock_step() -> Result<()>
  {
    let mut db = database();
    db.execute(
      "insert into sessions(id, name) values(7, 'implementer')",
      [],
    )?;

    let transaction = db.transaction()?;
    let original = create(&transaction, "inspect snapshots", 0, 20, None, None)?;
    let dispatched = dispatch(&transaction, original.id(), 7, false, None)?;
    let second_event = dispatched.events().last().expect("dispatch event");
    transaction.execute(
      "update task_events set created_at=? where id=?",
      [
        original.events()[0].created_at().timestamp_millis() - 1_000,
        second_event.id(),
      ],
    )?;
    let loaded = get(&transaction, original.id())?.expect("created task");
    transaction.commit()?;

    assert_eq!(
      loaded
        .events()
        .iter()
        .map(|event| event.id())
        .collect::<Vec<_>>(),
      vec![original.events()[0].id(), second_event.id()]
    );
    assert!(loaded.events()[1].created_at() < loaded.events()[0].created_at());
    assert_eq!(loaded.id(), original.id());
    Ok(())
  }

  #[test]
  fn reads_all_fully_materialized_tasks_in_ascending_identity_order() -> Result<()> {
    let mut db = database();
    db.execute(
      "insert into sessions(id, name) values(7, 'implementer')",
      [],
    )?;

    let transaction = db.transaction()?;
    assert!(all(&transaction)?.is_empty());

    let first = create(&transaction, "first", 0, 10, None, None)?;
    let first = dispatch(&transaction, first.id(), 7, false, None)?;
    let second = create(&transaction, "second", 0, 20, None, None)?;

    let tasks = all(&transaction)?;
    transaction.commit()?;

    assert_eq!(tasks, vec![first, second]);
    assert_eq!(tasks[0].events().len(), 2);
    assert_eq!(tasks[1].events().len(), 1);
    Ok(())
  }

  #[test]
  fn reads_fully_materialized_session_tasks_in_ascending_identity_order() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    transaction.execute_batch(
      "
        insert into sessions(id, name) values(7, 'target');
        insert into sessions(id, name) values(8, 'other');
        ",
    )?;
    assert!(tasks_for_session(&transaction, 7)?.is_empty());

    let first = create(&transaction, "first target task", 0, 10, None, None)?;
    let other = create(&transaction, "other session task", 0, 20, None, None)?;
    let second = create(&transaction, "second target task", 0, 30, None, None)?;
    transaction.execute(
      "update tasks set session_id=7 where id in (?1, ?2)",
      [first.id(), second.id()],
    )?;
    transaction.execute("update tasks set session_id=8 where id=?", [other.id()])?;
    task_event::create(&transaction, second.id(), TaskState::Dispatched, None)?;
    let first = get(&transaction, first.id())?.expect("first target task");
    let second = get(&transaction, second.id())?.expect("second target task");

    let tasks = tasks_for_session(&transaction, 7)?;
    transaction.commit()?;

    assert_eq!(tasks, vec![first, second]);
    assert_eq!(tasks[0].events().len(), 1);
    assert_eq!(tasks[1].events().len(), 2);
    Ok(())
  }

  #[test]
  fn reads_the_fully_materialized_immediate_identity_predecessor() -> Result<()> {
    let mut db = database();
    db.execute(
      "insert into sessions(id, name) values(7, 'implementer')",
      [],
    )?;

    let transaction = db.transaction()?;
    assert_eq!(predecessor(&transaction, 1)?, None);

    let first = create(&transaction, "first", 0, 10, None, None)?;
    let second = create(&transaction, "second", 0, 20, None, None)?;
    let third = create(&transaction, "third", 0, 30, None, None)?;
    transaction.execute("update tasks set session_id=7 where id=?", [second.id()])?;
    task_event::create(&transaction, second.id(), TaskState::Dispatched, None)?;
    let second = get(&transaction, second.id())?.expect("second task");

    assert_eq!(predecessor(&transaction, first.id())?, None);
    assert_eq!(predecessor(&transaction, third.id())?, Some(second.clone()));
    assert_eq!(predecessor(&transaction, i64::MAX)?, Some(third));
    assert_eq!(second.events().len(), 2);
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn mutators_update_task_data_and_append_the_paired_event() -> Result<()> {
    let mut db = database();
    db.execute(
      "insert into sessions(id, name) values(7, 'implementer')",
      [],
    )?;

    let transaction = db.transaction()?;
    let accepted = create(&transaction, "accept me", 0, 10, None, None)?;
    let accepted = dispatch(&transaction, accepted.id(), 7, true, Some("reuse"))?;
    assert_eq!(accepted.state(), TaskState::Dispatched);
    assert_eq!(accepted.session_id(), Some(7));
    assert!(accepted.is_session_reuse());
    assert_eq!(accepted.reason(), Some("reuse"));

    let accepted = take_flight(&transaction, accepted.id(), 42, "base123", 900)?;
    assert_eq!(accepted.state(), TaskState::InFlight);
    assert_eq!(accepted.log_offset(), 42);
    assert_eq!(accepted.base_head(), Some("base123"));
    assert_eq!(accepted.context_size_start(), Some(900));
    assert_eq!(accepted.reason(), None);

    let accepted = record_commit(&transaction, accepted.id(), "accepted123", None)?;
    assert_eq!(accepted.state(), TaskState::Committed);
    assert_eq!(accepted.commit_sha(), Some("accepted123"));
    let accepted = accept(&transaction, accepted.id(), "gate passed")?;
    assert_eq!(accepted.state(), TaskState::Accepted);
    assert_eq!(accepted.reason(), Some("gate passed"));
    assert_eq!(accepted.events().len(), 5);

    let aborted = create(&transaction, "abort me", 0, 10, None, None)?;
    let aborted = dispatch(&transaction, aborted.id(), 7, false, None)?;
    let aborted = abort(&transaction, aborted.id(), "implementation stalled")?;
    assert_eq!(aborted.state(), TaskState::Aborted);
    assert_eq!(aborted.reason(), Some("implementation stalled"));
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn advancing_to_the_current_state_is_a_noop() -> Result<()> {
    let mut db = database();
    db.execute(
      "insert into sessions(id, name) values(7, 'implementer')",
      [],
    )?;

    let transaction = db.transaction()?;
    let task = create(&transaction, "commit once", 0, 10, None, None)?;
    let task = dispatch(&transaction, task.id(), 7, false, None)?;
    let first = record_commit(&transaction, task.id(), "first123", None)?;
    let again = record_commit(&transaction, task.id(), "second456", None)?;
    transaction.commit()?;

    assert_eq!(first.state(), TaskState::Committed);
    assert_eq!(again, first);
    assert_eq!(again.commit_sha(), Some("first123"));
    assert_eq!(again.events().len(), 3);
    Ok(())
  }

  #[test]
  fn a_task_can_abort_from_every_state_it_reaches() -> Result<()> {
    let mut db = database();
    db.execute(
      "insert into sessions(id, name) values(7, 'implementer')",
      [],
    )?;

    let transaction = db.transaction()?;
    let drafted = create(&transaction, "abort while drafted", 0, 10, None, None)?;
    let drafted = abort(&transaction, drafted.id(), "spec withdrawn")?;

    let committed = create(&transaction, "abort after commit", 0, 10, None, None)?;
    let committed = dispatch(&transaction, committed.id(), 7, false, None)?;
    let committed = record_commit(&transaction, committed.id(), "landed123", None)?;
    let committed = abort(&transaction, committed.id(), "reverted, gate never ran")?;
    transaction.commit()?;

    assert_eq!(drafted.state(), TaskState::Aborted);
    assert_eq!(drafted.reason(), Some("spec withdrawn"));
    assert_eq!(committed.state(), TaskState::Aborted);
    assert_eq!(committed.commit_sha(), Some("landed123"));
    assert_eq!(committed.reason(), Some("reverted, gate never ran"));
    Ok(())
  }

  #[test]
  fn mutators_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    db.execute(
      "insert into sessions(id, name) values(7, 'implementer')",
      [],
    )?;
    let transaction = db.transaction()?;
    let task = create(&transaction, "remain drafted", 0, 10, None, None)?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    dispatch(&transaction, task.id(), 7, false, None)?;
    transaction.rollback()?;

    let transaction = db.transaction()?;
    let task = get(&transaction, task.id())?.expect("drafted task");
    transaction.commit()?;
    assert_eq!(task.state(), TaskState::Drafted);
    assert_eq!(task.session_id(), None);
    assert_eq!(task.events().len(), 1);
    Ok(())
  }

  #[test]
  fn mutators_reject_backward_transitions_before_changing_task_data() -> Result<()> {
    let mut db = database();
    db.execute(
      "insert into sessions(id, name) values(7, 'implementer')",
      [],
    )?;

    let transaction = db.transaction()?;
    let task = create(&transaction, "stay aborted", 0, 10, None, None)?;
    let task = dispatch(&transaction, task.id(), 7, false, None)?;
    let task = abort(&transaction, task.id(), "implementation failed")?;

    let error = record_commit(&transaction, task.id(), "unexpected123", None).unwrap_err();
    let unchanged = get(&transaction, task.id())?.expect("aborted task");
    transaction.commit()?;

    assert_eq!(
      error.to_string(),
      "task 1 cannot advance from aborted to committed"
    );
    assert_eq!(unchanged.state(), TaskState::Aborted);
    assert_eq!(unchanged.commit_sha(), None);
    assert_eq!(unchanged.events().len(), 3);
    Ok(())
  }
}
