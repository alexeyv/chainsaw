use anyhow::Result;
use rusqlite::{Connection, params};

use crate::domain::{TaskEvent, TaskState};

pub fn create(db: &mut Connection, task_id: i64, state: TaskState, at: f64) -> Result<TaskEvent> {
  let transaction = db.transaction()?;
  let id = transaction.query_row(
    "
      insert into task_events(task_id, state, at)
      values (?1, ?2, ?3)
      returning rowid
      ",
    params![task_id, state_name(state), at],
    |row| row.get(0),
  )?;
  let event = TaskEvent::new(id, state, at)?;
  transaction.commit()?;
  Ok(event)
}

fn state_name(state: TaskState) -> &'static str {
  match state {
    TaskState::Drafted => "drafted",
    TaskState::Dispatched => "dispatched",
    TaskState::InFlight => "in_flight",
    TaskState::Committed => "committed",
    TaskState::Verified => "verified",
    TaskState::Accepted => "accepted",
    TaskState::Ingested => "ingested",
    TaskState::Failed => "failed",
  }
}

#[cfg(test)]
mod tests {
  use anyhow::Result;

  use super::create;
  use crate::domain::TaskState;
  use crate::persistence::test_fixture::database;

  #[test]
  fn creates_a_valid_task_event() -> Result<()> {
    let mut db = database();

    let event = create(&mut db, 7, TaskState::InFlight, 1_700_000_000.0)?;

    let stored = db.query_row(
      "select task_id, state, at from task_events where rowid=?",
      [event.id()],
      |row| {
        Ok((
          row.get::<_, i64>(0)?,
          row.get::<_, String>(1)?,
          row.get::<_, f64>(2)?,
        ))
      },
    )?;
    assert_eq!(event.state(), TaskState::InFlight);
    assert_eq!(event.at(), 1_700_000_000.0);
    assert_eq!(stored, (7, "in_flight".to_owned(), 1_700_000_000.0));
    Ok(())
  }

  #[test]
  fn rolls_back_when_domain_validation_fails() -> Result<()> {
    let mut db = database();

    let error = create(&mut db, 7, TaskState::Drafted, -1.0).unwrap_err();

    let count = db.query_row("select count(*) from task_events", [], |row| {
      row.get::<_, i64>(0)
    })?;
    assert_eq!(error.to_string(), "at must be finite and nonnegative");
    assert_eq!(count, 0);
    Ok(())
  }
}
