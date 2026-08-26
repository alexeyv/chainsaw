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
