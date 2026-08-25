use anyhow::Result;
use chrono::Utc;
use rusqlite::{Transaction, params};

use crate::domain::{TaskEvent, TaskState};

pub fn create(transaction: &Transaction<'_>, task_id: i64, state: TaskState) -> Result<TaskEvent> {
  let created_at = Utc::now();
  let id = transaction.query_row(
    "
      insert into task_events(task_id, state, created_at)
      values (?1, ?2, ?3)
      returning id
      ",
    params![task_id, state.as_str(), created_at.timestamp_millis()],
    |row| row.get(0),
  )?;
  let event = TaskEvent::new(id, state, created_at)?;
  Ok(event)
}

#[cfg(test)]
mod tests {
  use anyhow::Result;
  use chrono::Utc;
  use rusqlite::Connection;

  use super::create;
  use crate::domain::TaskState;
  use crate::persistence::test_fixture::database;

  fn create_task(db: &Connection, id: i64) -> Result<()> {
    db.execute("insert into tasks(id) values(?)", [id])?;
    Ok(())
  }

  #[test]
  fn creates_a_valid_task_event() -> Result<()> {
    let mut db = database();
    create_task(&db, 7)?;

    let before = Utc::now();
    let transaction = db.transaction()?;
    let event = create(&transaction, 7, TaskState::InFlight)?;
    transaction.commit()?;
    let after = Utc::now();

    let stored = db.query_row(
      "select task_id, state, created_at from task_events where id=?",
      [event.id()],
      |row| {
        Ok((
          row.get::<_, i64>(0)?,
          row.get::<_, String>(1)?,
          row.get::<_, i64>(2)?,
        ))
      },
    )?;
    assert_eq!(event.state(), TaskState::InFlight);
    assert!(event.created_at() >= before);
    assert!(event.created_at() <= after);
    assert_eq!(
      stored,
      (
        7,
        "in_flight".to_owned(),
        event.created_at().timestamp_millis()
      )
    );
    Ok(())
  }

  #[test]
  fn assigns_autoincrementing_ids() -> Result<()> {
    let mut db = database();
    create_task(&db, 7)?;

    let transaction = db.transaction()?;
    let first = create(&transaction, 7, TaskState::Drafted)?;
    let second = create(&transaction, 7, TaskState::Dispatched)?;
    transaction.commit()?;

    assert_eq!(first.id(), 1);
    assert_eq!(second.id(), 2);
    Ok(())
  }

  #[test]
  fn leaves_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    create_task(&db, 7)?;

    let transaction = db.transaction()?;
    create(&transaction, 7, TaskState::Drafted)?;
    transaction.rollback()?;

    let count = db.query_row("select count(*) from task_events", [], |row| {
      row.get::<_, i64>(0)
    })?;
    assert_eq!(count, 0);
    Ok(())
  }

  #[test]
  fn rejects_an_event_for_a_missing_task() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(&transaction, 7, TaskState::Drafted).unwrap_err();
    transaction.rollback()?;

    let count = db.query_row("select count(*) from task_events", [], |row| {
      row.get::<_, i64>(0)
    })?;
    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    assert_eq!(count, 0);
    Ok(())
  }
}
