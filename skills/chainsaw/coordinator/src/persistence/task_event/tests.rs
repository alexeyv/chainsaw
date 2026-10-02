use anyhow::Result;
use chrono::Utc;

use super::create;
use crate::domain::TaskState;
use crate::persistence::test_fixture::{database, row_count, task_row};

mod create {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let before = Utc::now();
    let transaction = db.transaction()?;
    let event = create(&transaction, 7, TaskState::InFlight, Some("reuse"))?;
    transaction.commit()?;
    let after = Utc::now();

    let stored = db.query_row(
      "select task_id, state, reason, created_at from task_events where id=?",
      [event.id()],
      |row| {
        Ok((
          row.get::<_, i64>(0)?,
          row.get::<_, String>(1)?,
          row.get::<_, Option<String>>(2)?,
          row.get::<_, i64>(3)?,
        ))
      },
    )?;
    assert_eq!(event.id(), 1);
    assert_eq!(event.state(), TaskState::InFlight);
    assert_eq!(event.reason(), Some("reuse"));
    assert!(event.created_at() >= before);
    assert!(event.created_at() <= after);
    assert_eq!(
      stored,
      (
        7,
        "in_flight".to_owned(),
        Some("reuse".to_owned()),
        event.created_at().timestamp_millis()
      )
    );
    Ok(())
  }

  #[test]
  fn should_store_no_reason_when_none_is_given() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    let event = create(&transaction, 7, TaskState::Drafted, None)?;
    transaction.commit()?;

    let stored: Option<String> = db.query_row(
      "select reason from task_events where id=?",
      [event.id()],
      |row| row.get(0),
    )?;
    assert_eq!(event.reason(), None);
    assert_eq!(stored, None);
    Ok(())
  }

  #[test]
  fn should_assign_increasing_ids() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    let first = create(&transaction, 7, TaskState::Drafted, None)?;
    let second = create(&transaction, 7, TaskState::Dispatched, None)?;
    transaction.commit()?;

    assert_eq!((first.id(), second.id()), (1, 2));
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    create(&transaction, 7, TaskState::Drafted, None)?;
    transaction.rollback()?;

    assert_eq!(row_count(&db, "task_events")?, 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_does_not_exist() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(&transaction, 7, TaskState::Drafted, None).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    assert_eq!(row_count(&db, "task_events")?, 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_reason_is_blank() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    let error = create(&transaction, 7, TaskState::Aborted, Some(" ")).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "reason cannot be blank");
    Ok(())
  }
}
