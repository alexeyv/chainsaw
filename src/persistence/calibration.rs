use anyhow::Result;
use chrono::Utc;
use rusqlite::{Transaction, params};

use crate::domain::Calibration;

#[allow(clippy::too_many_arguments)]
pub fn create(
  transaction: &Transaction<'_>,
  task_id: i64,
  predicted_files: i64,
  predicted_lines: i64,
  actual_files: i64,
  actual_lines: i64,
  wall_seconds: Option<f64>,
  context_size_start: i64,
  context_size_end: i64,
) -> Result<Calibration> {
  let created_at = Utc::now();
  let id = transaction.query_row(
    "
      insert into calibrations(
        task_id, predicted_files, predicted_lines, actual_files, actual_lines,
        wall_seconds, created_at, context_size_start, context_size_end
      ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
      returning id
      ",
    params![
      task_id,
      predicted_files,
      predicted_lines,
      actual_files,
      actual_lines,
      wall_seconds,
      created_at.timestamp_millis(),
      context_size_start,
      context_size_end,
    ],
    |row| row.get(0),
  )?;
  let calibration = Calibration::new(
    id,
    task_id,
    predicted_files,
    predicted_lines,
    actual_files,
    actual_lines,
    wall_seconds,
    created_at,
    context_size_start,
    context_size_end,
  )?;
  Ok(calibration)
}

#[cfg(test)]
mod tests {
  use anyhow::Result;
  use chrono::Utc;
  use rusqlite::Connection;

  use super::create;
  use crate::persistence::test_fixture::database;

  fn create_task(db: &Connection, id: i64) -> Result<()> {
    db.execute("insert into tasks(id) values(?)", [id])?;
    Ok(())
  }

  #[test]
  fn creates_a_valid_calibration() -> Result<()> {
    let mut db = database();
    create_task(&db, 7)?;

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
    assert!(calibration.created_at() >= before);
    assert!(calibration.created_at() <= after);
    assert_eq!(calibration.task_id(), 7);
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
  fn assigns_autoincrementing_ids() -> Result<()> {
    let mut db = database();
    create_task(&db, 7)?;
    create_task(&db, 8)?;

    let transaction = db.transaction()?;
    let first = create(&transaction, 7, 0, 0, 0, 0, None, 0, 0)?;
    let second = create(&transaction, 8, 0, 0, 0, 0, None, 0, 0)?;
    transaction.commit()?;

    assert_eq!(first.id(), 1);
    assert_eq!(second.id(), 2);
    Ok(())
  }

  #[test]
  fn allows_only_one_calibration_per_task() -> Result<()> {
    let mut db = database();
    create_task(&db, 7)?;

    let transaction = db.transaction()?;
    create(&transaction, 7, 0, 0, 0, 0, None, 0, 0)?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    let error = create(&transaction, 7, 0, 0, 0, 0, None, 0, 0).unwrap_err();
    transaction.rollback()?;

    let count = db.query_row("select count(*) from calibrations", [], |row| {
      row.get::<_, i64>(0)
    })?;
    assert_eq!(
      error.to_string(),
      "UNIQUE constraint failed: calibrations.task_id"
    );
    assert_eq!(count, 1);
    Ok(())
  }

  #[test]
  fn leaves_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    create_task(&db, 7)?;

    let transaction = db.transaction()?;
    create(&transaction, 7, 0, 0, 0, 0, None, 0, 0)?;
    transaction.rollback()?;

    let count = db.query_row("select count(*) from calibrations", [], |row| {
      row.get::<_, i64>(0)
    })?;
    assert_eq!(count, 0);
    Ok(())
  }

  #[test]
  fn rejects_invalid_data_before_commit() -> Result<()> {
    let mut db = database();
    create_task(&db, 7)?;

    let transaction = db.transaction()?;
    let error = create(&transaction, 7, -1, 0, 0, 0, None, 0, 0).unwrap_err();
    transaction.rollback()?;

    let count = db.query_row("select count(*) from calibrations", [], |row| {
      row.get::<_, i64>(0)
    })?;
    assert_eq!(error.to_string(), "predicted_files cannot be negative");
    assert_eq!(count, 0);
    Ok(())
  }

  #[test]
  fn rejects_a_calibration_for_a_missing_task() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(&transaction, 7, 0, 0, 0, 0, None, 0, 0).unwrap_err();
    transaction.rollback()?;

    let count = db.query_row("select count(*) from calibrations", [], |row| {
      row.get::<_, i64>(0)
    })?;
    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    assert_eq!(count, 0);
    Ok(())
  }
}
