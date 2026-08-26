use anyhow::Result;
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, params};

pub fn record(transaction: &Transaction<'_>, task_id: i64) -> Result<bool> {
  let delivered_at = Utc::now().timestamp_millis();
  let changed = transaction.execute(
    "insert or ignore into commentary_deliveries(task_id, delivered_at) values(?, ?)",
    params![task_id, delivered_at],
  )?;
  Ok(changed == 1)
}

pub fn delivered_at(transaction: &Transaction<'_>, task_id: i64) -> Result<Option<i64>> {
  transaction
    .query_row(
      "select delivered_at from commentary_deliveries where task_id=?",
      [task_id],
      |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
  use anyhow::Result;

  use super::{delivered_at, record};
  use crate::persistence::test_fixture::database;

  #[test]
  fn records_delivery_once_without_changing_the_task_lifecycle() -> Result<()> {
    let mut db = database();
    db.execute("insert into tasks(id) values(7)", [])?;

    let transaction = db.transaction()?;
    assert!(record(&transaction, 7)?);
    assert!(!record(&transaction, 7)?);
    assert!(delivered_at(&transaction, 7)?.is_some());
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn leaves_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    db.execute("insert into tasks(id) values(7)", [])?;

    let transaction = db.transaction()?;
    record(&transaction, 7)?;
    transaction.rollback()?;

    let transaction = db.transaction()?;
    assert_eq!(delivered_at(&transaction, 7)?, None);
    transaction.commit()?;
    Ok(())
  }
}
