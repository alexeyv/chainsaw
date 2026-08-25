use anyhow::Result;
use chrono::Utc;
use rusqlite::{Transaction, params};

use crate::domain::{Task, TaskState};

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
  let event = super::task_event::create(transaction, id, TaskState::Drafted)?;
  let task = Task::new(
    id,
    text.to_owned(),
    predicted_files,
    predicted_lines,
    None,
    None,
    created_at,
    retry_of_task_id,
    None,
    0,
    None,
    predicted_file_list,
    false,
    None,
    None,
    vec![event],
  )?;
  Ok(task)
}

#[cfg(test)]
mod tests {
  use anyhow::Result;
  use chrono::Utc;

  use super::create;
  use crate::domain::TaskState;
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
}
