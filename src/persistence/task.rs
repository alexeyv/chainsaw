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
  reason: Option<String>,
  log_offset: i64,
  base_head: Option<String>,
  predicted_file_list: Option<Vec<String>>,
  is_session_reuse: bool,
  context_size_start: Option<i64>,
  context_size_end: Option<i64>,
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
  super::task_event::create(transaction, id, TaskState::Drafted)?;
  get(transaction, id)?.with_context(|| format!("created task {id} is missing"))
}

pub fn get(transaction: &Transaction<'_>, id: i64) -> Result<Option<Task>> {
  let row = transaction
    .query_row(
      "
        select id, text, predicted_files, predicted_lines, session_id,
               commit_sha, created_at, retry_of_task_id, reason, log_offset,
               base_head, predicted_file_list, is_session_reuse,
               context_size_start, context_size_end
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
               commit_sha, created_at, retry_of_task_id, reason, log_offset,
               base_head, predicted_file_list, is_session_reuse,
               context_size_start, context_size_end
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
               commit_sha, created_at, retry_of_task_id, reason, log_offset,
               base_head, predicted_file_list, is_session_reuse,
               context_size_start, context_size_end
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
               commit_sha, created_at, retry_of_task_id, reason, log_offset,
               base_head, predicted_file_list, is_session_reuse,
               context_size_start, context_size_end
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
) -> Result<Task> {
  transition(transaction, id, TaskState::Dispatched, |transaction| {
    transaction.execute(
      "update tasks set session_id=?, is_session_reuse=? where id=?",
      params![session_id, i64::from(reuse), id],
    )?;
    Ok(())
  })
}

pub fn redraft(transaction: &Transaction<'_>, id: i64) -> Result<Task> {
  transition(transaction, id, TaskState::Drafted, |_| Ok(()))
}

pub fn take_flight(
  transaction: &Transaction<'_>,
  id: i64,
  log_offset: i64,
  base_head: &str,
  context_size_start: i64,
) -> Result<Task> {
  transition(transaction, id, TaskState::InFlight, |transaction| {
    transaction.execute(
      "update tasks set log_offset=?, base_head=?, context_size_start=? where id=?",
      params![log_offset, base_head, context_size_start, id],
    )?;
    Ok(())
  })
}

pub fn verify(transaction: &Transaction<'_>, id: i64, commit_sha: &str) -> Result<Task> {
  transition(transaction, id, TaskState::Verified, |transaction| {
    transaction.execute(
      "update tasks set commit_sha=? where id=?",
      params![commit_sha, id],
    )?;
    Ok(())
  })
}

pub fn accept(transaction: &Transaction<'_>, id: i64, reason: &str) -> Result<Task> {
  transition(transaction, id, TaskState::Accepted, |transaction| {
    transaction.execute("update tasks set reason=? where id=?", params![reason, id])?;
    Ok(())
  })
}

pub fn fail(transaction: &Transaction<'_>, id: i64, reason: &str) -> Result<Task> {
  transition(transaction, id, TaskState::Failed, |transaction| {
    transaction.execute("update tasks set reason=? where id=?", params![reason, id])?;
    Ok(())
  })
}

pub fn record_commit(transaction: &Transaction<'_>, id: i64, commit_sha: &str) -> Result<Task> {
  transition(transaction, id, TaskState::Committed, |transaction| {
    transaction.execute(
      "update tasks set commit_sha=? where id=?",
      params![commit_sha, id],
    )?;
    Ok(())
  })
}

fn transition(
  transaction: &Transaction<'_>,
  id: i64,
  next: TaskState,
  mutate: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<Task> {
  let current = get(transaction, id)?.with_context(|| format!("task {id} is missing"))?;
  if !current.state().can_transition_to(next) {
    bail!(
      "task {id} cannot transition from {} to {next}",
      current.state()
    );
  }
  mutate(transaction)?;
  super::task_event::create(transaction, id, next)?;
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
    reason: row.get("reason")?,
    log_offset: row.get::<_, Option<i64>>("log_offset")?.unwrap_or_default(),
    base_head: row.get("base_head")?,
    predicted_file_list,
    is_session_reuse: row.get::<_, i64>("is_session_reuse")? != 0,
    context_size_start: row.get("context_size_start")?,
    context_size_end: row.get("context_size_end")?,
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
    row.reason,
    row.log_offset,
    row.base_head,
    row.predicted_file_list,
    row.is_session_reuse,
    row.context_size_start,
    row.context_size_end,
    events,
  )
}

fn load_events(transaction: &Transaction<'_>, task_id: i64) -> Result<Vec<TaskEvent>> {
  let mut statement = transaction.prepare(
    "
      select id, state, created_at
      from task_events where task_id=? order by id
      ",
  )?;
  let rows = statement.query_map([task_id], |row| {
    Ok((
      row.get::<_, i64>(0)?,
      row.get::<_, String>(1)?,
      row.get::<_, i64>(2)?,
    ))
  })?;
  rows
    .map(|row| {
      let (id, state, created_at) = row?;
      let state = TaskState::try_from(state.as_str())?;
      let created_at = DateTime::from_timestamp_millis(created_at)
        .context("task event created_at is outside the supported range")?;
      TaskEvent::new(id, state, created_at)
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use anyhow::Result;
  use chrono::Utc;

  use super::{
    accept, all, create, dispatch, fail, get, predecessor, record_commit, redraft, take_flight,
    tasks_for_session, verify,
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
    let dispatched = dispatch(&transaction, original.id(), 7, false)?;
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
    let first = dispatch(&transaction, first.id(), 7, false)?;
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
    task_event::create(&transaction, second.id(), TaskState::Dispatched)?;
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
    task_event::create(&transaction, second.id(), TaskState::Dispatched)?;
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
    let verified = create(&transaction, "verify me", 0, 10, None, None)?;
    let verified = dispatch(&transaction, verified.id(), 7, true)?;
    assert_eq!(verified.state(), TaskState::Dispatched);
    assert_eq!(verified.session_id(), Some(7));
    assert!(verified.is_session_reuse());

    let verified = redraft(&transaction, verified.id())?;
    assert_eq!(verified.state(), TaskState::Drafted);
    let verified = dispatch(&transaction, verified.id(), 7, true)?;
    let verified = take_flight(&transaction, verified.id(), 42, "base123", 900)?;
    assert_eq!(verified.state(), TaskState::InFlight);
    assert_eq!(verified.log_offset(), 42);
    assert_eq!(verified.base_head(), Some("base123"));
    assert_eq!(verified.context_size_start(), Some(900));

    let verified = verify(&transaction, verified.id(), "verified123")?;
    assert_eq!(verified.state(), TaskState::Verified);
    assert_eq!(verified.commit_sha(), Some("verified123"));
    assert_eq!(verified.events().len(), 6);

    let accepted = create(&transaction, "accept me", 0, 10, None, None)?;
    let accepted = dispatch(&transaction, accepted.id(), 7, false)?;
    let accepted = record_commit(&transaction, accepted.id(), "accepted123")?;
    assert_eq!(accepted.state(), TaskState::Committed);
    assert_eq!(accepted.commit_sha(), Some("accepted123"));
    let accepted = accept(&transaction, accepted.id(), "manual review")?;
    assert_eq!(accepted.state(), TaskState::Accepted);
    assert_eq!(accepted.reason(), Some("manual review"));
    let failed = create(&transaction, "fail me", 0, 10, None, None)?;
    let failed = dispatch(&transaction, failed.id(), 7, false)?;
    let failed = fail(&transaction, failed.id(), "implementation stalled")?;
    assert_eq!(failed.state(), TaskState::Failed);
    assert_eq!(failed.reason(), Some("implementation stalled"));
    transaction.commit()?;
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
    dispatch(&transaction, task.id(), 7, false)?;
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
  fn mutators_reject_illegal_transitions_before_changing_task_data() -> Result<()> {
    let mut db = database();
    db.execute(
      "insert into sessions(id, name) values(7, 'implementer')",
      [],
    )?;

    let transaction = db.transaction()?;
    let task = create(&transaction, "stay failed", 0, 10, None, None)?;
    let task = dispatch(&transaction, task.id(), 7, false)?;
    let task = fail(&transaction, task.id(), "implementation failed")?;

    let error = verify(&transaction, task.id(), "unexpected123").unwrap_err();
    let unchanged = get(&transaction, task.id())?.expect("failed task");
    transaction.commit()?;

    assert_eq!(
      error.to_string(),
      "task 1 cannot transition from failed to verified"
    );
    assert_eq!(unchanged.state(), TaskState::Failed);
    assert_eq!(unchanged.commit_sha(), None);
    assert_eq!(unchanged.events().len(), 3);
    Ok(())
  }
}
