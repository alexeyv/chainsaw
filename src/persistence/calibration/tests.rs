use anyhow::Result;
use chrono::Utc;

use super::create;
use crate::persistence::test_fixture::{database, row_count, task_row};

mod create {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let before = Utc::now();
    let transaction = db.transaction()?;
    let calibration = create(&transaction, 7, 2, 20, 4, 35, Some(12.5), 100, 900)?;
    transaction.commit()?;
    let after = Utc::now();

    let stored = db.query_row(
      "
        select task_id, predicted_files, predicted_lines, actual_files,
          actual_lines, wall_seconds, created_at, context_size_start,
          context_size_end
        from calibrations where id=?
        ",
      [calibration.id()],
      |row| {
        Ok((
          row.get::<_, i64>(0)?,
          row.get::<_, i64>(1)?,
          row.get::<_, i64>(2)?,
          row.get::<_, i64>(3)?,
          row.get::<_, i64>(4)?,
          row.get::<_, Option<f64>>(5)?,
          row.get::<_, i64>(6)?,
          row.get::<_, i64>(7)?,
          row.get::<_, i64>(8)?,
        ))
      },
    )?;
    assert_eq!(calibration.id(), 1);
    assert_eq!(calibration.task_id(), 7);
    assert!(calibration.created_at() >= before);
    assert!(calibration.created_at() <= after);
    assert_eq!(
      stored,
      (
        7,
        2,
        20,
        4,
        35,
        Some(12.5),
        calibration.created_at().timestamp_millis(),
        100,
        900,
      )
    );
    Ok(())
  }

  #[test]
  fn should_assign_increasing_ids_across_tasks() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;
    task_row(&db, 8)?;

    let transaction = db.transaction()?;
    let first = create(&transaction, 7, 0, 0, 0, 0, None, 0, 0)?;
    let second = create(&transaction, 8, 0, 0, 0, 0, None, 0, 0)?;
    transaction.commit()?;

    assert_eq!((first.id(), second.id()), (1, 2));
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    create(&transaction, 7, 0, 0, 0, 0, None, 0, 0)?;
    transaction.rollback()?;

    assert_eq!(row_count(&db, "calibrations")?, 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_already_has_a_calibration() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;
    let transaction = db.transaction()?;
    create(&transaction, 7, 0, 0, 0, 0, None, 0, 0)?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    let error = create(&transaction, 7, 0, 0, 0, 0, None, 0, 0).unwrap_err();
    transaction.rollback()?;

    assert_eq!(
      error.to_string(),
      "UNIQUE constraint failed: calibrations.task_id"
    );
    assert_eq!(row_count(&db, "calibrations")?, 1);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_does_not_exist() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(&transaction, 7, 0, 0, 0, 0, None, 0, 0).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    assert_eq!(row_count(&db, "calibrations")?, 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_measurements_are_invalid_after_the_row_is_written() -> Result<()> {
    let mut db = database();
    task_row(&db, 7)?;

    let transaction = db.transaction()?;
    let error = create(&transaction, 7, -1, 0, 0, 0, None, 0, 0).unwrap_err();
    assert_eq!(error.to_string(), "predicted_files cannot be negative");
    transaction.rollback()?;

    assert_eq!(row_count(&db, "calibrations")?, 0);
    Ok(())
  }
}
