use anyhow::Result;
use chrono::Utc;

use super::{delivered_at, record, record_wake, woken_at};
use crate::persistence::test_fixture::{database, row_count, task_row};

mod record {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let before = Utc::now().timestamp_millis();
    let transaction = db.transaction()?;
    let recorded = record(&transaction, 7)?;
    let stored = delivered_at(&transaction, 7)?;
    transaction.commit()?;
    let after = Utc::now().timestamp_millis();

    assert!(recorded);
    let stored = stored.expect("delivery timestamp");
    let stored = stored.timestamp_millis();
    assert!(stored >= before && stored <= after);
    Ok(())
  }

  #[test]
  fn should_keep_the_first_delivery_when_recorded_again() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    record(&transaction, 7)?;
    let first = delivered_at(&transaction, 7)?;
    let recorded_again = record(&transaction, 7)?;
    let second = delivered_at(&transaction, 7)?;
    transaction.commit()?;

    assert!(!recorded_again);
    assert_eq!(second, first);
    assert_eq!(row_count(&db, "commentary_deliveries")?, 1);
    Ok(())
  }

  #[test]
  fn should_deliver_a_woken_task_on_the_same_row() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    record_wake(&transaction, 7)?;
    let wake = woken_at(&transaction, 7)?;
    let recorded = record(&transaction, 7)?;
    let delivery = delivered_at(&transaction, 7)?;
    transaction.commit()?;

    assert!(recorded);
    assert!(wake.is_some());
    assert!(delivery.is_some());
    assert_eq!(row_count(&db, "commentary_deliveries")?, 1);
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    record(&transaction, 7)?;
    transaction.rollback()?;

    assert_eq!(row_count(&db, "commentary_deliveries")?, 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_does_not_exist() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = record(&transaction, 7).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    Ok(())
  }
}

mod delivered_at {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    record(&transaction, 7)?;
    let stored = delivered_at(&transaction, 7)?;
    transaction.commit()?;

    let raw: i64 = db.query_row(
      "select delivered_at from commentary_deliveries where task_id=7",
      [],
      |row| row.get(0),
    )?;
    assert_eq!(stored.map(|at| at.timestamp_millis()), Some(raw));
    Ok(())
  }

  #[test]
  fn should_return_none_when_nothing_was_delivered() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    assert_eq!(delivered_at(&transaction, 7)?, None);
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_return_none_when_the_task_was_only_woken() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    record_wake(&transaction, 7)?;
    assert_eq!(delivered_at(&transaction, 7)?, None);
    transaction.commit()?;
    Ok(())
  }
}

mod record_wake {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let before = Utc::now().timestamp_millis();
    let transaction = db.transaction()?;
    let recorded = record_wake(&transaction, 7)?;
    let stored = woken_at(&transaction, 7)?;
    transaction.commit()?;
    let after = Utc::now().timestamp_millis();

    assert!(recorded);
    let stored = stored.expect("wake timestamp");
    let stored = stored.timestamp_millis();
    assert!(stored >= before && stored <= after);
    assert_eq!(row_count(&db, "commentary_deliveries")?, 1);
    Ok(())
  }

  #[test]
  fn should_keep_the_first_wake_when_recorded_again() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    record_wake(&transaction, 7)?;
    let first = woken_at(&transaction, 7)?;
    let recorded_again = record_wake(&transaction, 7)?;
    let second = woken_at(&transaction, 7)?;
    transaction.commit()?;

    assert!(!recorded_again);
    assert_eq!(second, first);
    assert_eq!(row_count(&db, "commentary_deliveries")?, 1);
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    record_wake(&transaction, 7)?;
    transaction.rollback()?;

    assert_eq!(row_count(&db, "commentary_deliveries")?, 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_does_not_exist() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = record_wake(&transaction, 7).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    Ok(())
  }
}

mod woken_at {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    record_wake(&transaction, 7)?;
    let stored = woken_at(&transaction, 7)?;
    transaction.commit()?;

    let raw: i64 = db.query_row(
      "select woken_at from commentary_deliveries where task_id=7",
      [],
      |row| row.get(0),
    )?;
    assert_eq!(stored.map(|at| at.timestamp_millis()), Some(raw));
    Ok(())
  }

  #[test]
  fn should_return_none_when_nothing_was_woken() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    assert_eq!(woken_at(&transaction, 7)?, None);
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_return_none_when_the_task_was_only_delivered() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    record(&transaction, 7)?;
    assert_eq!(woken_at(&transaction, 7)?, None);
    transaction.commit()?;
    Ok(())
  }
}
