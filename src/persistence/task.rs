use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::domain::{FindingVerdict, Task, TaskEvent, TaskState};

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
        text, predicted_files, predicted_lines, state, created_at,
        retry_of_task_id, predicted_file_list
      ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
      returning id
      ",
    params![
      text,
      predicted_files,
      predicted_lines,
      TaskState::Drafted.as_str(),
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

pub fn record_finding(
  transaction: &Transaction<'_>,
  task: &Task,
  description: &str,
  verdict: FindingVerdict,
  verdict_reason: &str,
  fix_task_id: Option<i64>,
) -> Result<Task> {
  super::finding::insert(
    transaction,
    task.id(),
    description,
    verdict,
    verdict_reason,
    fix_task_id,
  )?;
  get(transaction, task.id())?
    .with_context(|| format!("task {} disappeared while recording a finding", task.id()))
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
  let findings = super::finding::for_task(transaction, row.id)?;
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
    findings,
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

  use super::{all, create, get, record_finding};
  use crate::domain::{FindingVerdict, TaskState};
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
        select text, predicted_files, predicted_lines, state, created_at,
               retry_of_task_id, predicted_file_list
        from tasks where id=?
        ",
      [task.id()],
      |row| {
        Ok((
          row.get::<_, String>(0)?,
          row.get::<_, i64>(1)?,
          row.get::<_, i64>(2)?,
          row.get::<_, String>(3)?,
          row.get::<_, f64>(4)?,
          row.get::<_, Option<i64>>(5)?,
          row.get::<_, Option<String>>(6)?,
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
    assert!(task.findings().is_empty());
    assert_eq!(
      stored,
      (
        "materialize tasks through persistence".to_owned(),
        2,
        80,
        "drafted".to_owned(),
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
  fn reads_a_fully_materialized_task_in_stable_identity_order() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let original = create(&transaction, "inspect snapshots", 0, 20, None, None)?;
    let second_event = task_event::create(&transaction, original.id(), TaskState::Drafted)?;
    let first_snapshot = record_finding(
      &transaction,
      &original,
      "first finding",
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    let second_snapshot = record_finding(
      &transaction,
      &first_snapshot,
      "second finding",
      FindingVerdict::Dropped,
      "already covered",
      None,
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
    assert_eq!(
      loaded
        .findings()
        .iter()
        .map(|finding| finding.id())
        .collect::<Vec<_>>(),
      vec![
        first_snapshot.findings()[0].id(),
        second_snapshot.findings()[1].id()
      ]
    );
    assert_eq!(loaded, second_snapshot);
    Ok(())
  }

  #[test]
  fn reads_all_fully_materialized_tasks_in_ascending_identity_order() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    assert!(all(&transaction)?.is_empty());

    let first = create(&transaction, "first", 0, 10, None, None)?;
    task_event::create(&transaction, first.id(), TaskState::Drafted)?;
    let first = record_finding(
      &transaction,
      &first,
      "first task finding",
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    let second = create(&transaction, "second", 0, 20, None, None)?;
    let second = record_finding(
      &transaction,
      &second,
      "second task finding",
      FindingVerdict::Dropped,
      "already covered",
      None,
    )?;

    let tasks = all(&transaction)?;
    transaction.commit()?;

    assert_eq!(tasks, vec![first, second]);
    assert_eq!(tasks[0].events().len(), 2);
    assert_eq!(tasks[0].findings().len(), 1);
    assert_eq!(tasks[1].events().len(), 1);
    assert_eq!(tasks[1].findings().len(), 1);
    Ok(())
  }

  #[test]
  fn recording_a_finding_returns_a_new_snapshot_and_commits_it() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let original = create(&transaction, "keep snapshots immutable", 0, 10, None, None)?;
    transaction.commit()?;

    let original_snapshot = original.clone();
    let transaction = db.transaction()?;
    let updated = record_finding(
      &transaction,
      &original,
      "a defect",
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;

    assert_eq!(original, original_snapshot);
    assert!(original.findings().is_empty());
    assert_eq!(updated.findings().len(), 1);
    assert_eq!(updated.findings()[0].task_id(), original.id());
    transaction.commit()?;

    let transaction = db.transaction()?;
    assert_eq!(get(&transaction, original.id())?, Some(updated));
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn rolling_back_a_recorded_finding_preserves_the_stored_snapshot() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let original = create(&transaction, "roll back findings", 0, 10, None, None)?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    let updated = record_finding(
      &transaction,
      &original,
      "temporary finding",
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    assert_eq!(updated.findings().len(), 1);
    transaction.rollback()?;

    let transaction = db.transaction()?;
    let loaded = get(&transaction, original.id())?.expect("created task");
    transaction.commit()?;

    assert_eq!(loaded, original);
    assert!(loaded.findings().is_empty());
    Ok(())
  }
}
