use anyhow::Result;

use super::{after, create};
use crate::persistence::test_fixture::database;

#[test]
fn records_and_incrementally_reads_chronological_observations() -> Result<()> {
  let mut db = database();
  db.execute("insert into tasks(id) values(5)", [])?;

  let transaction = db.transaction()?;
  let first = create(&transaction, Some(5), "first")?;
  let second = create(&transaction, None, "second")?;
  transaction.commit()?;

  assert_eq!(first.id(), 1);
  assert_eq!(first.task_id(), Some(5));
  assert_eq!(first.text(), "first");
  assert_eq!(after(&db, first.id(), None)?, vec![second]);
  Ok(())
}

#[test]
fn task_filter_includes_run_wide_observations_but_not_other_tasks() -> Result<()> {
  let mut db = database();
  db.execute_batch("insert into tasks(id) values(5); insert into tasks(id) values(6);")?;
  let transaction = db.transaction()?;
  let relevant = create(&transaction, Some(5), "relevant")?;
  create(&transaction, Some(6), "other task")?;
  let run_wide = create(&transaction, None, "run wide")?;
  transaction.commit()?;

  assert_eq!(after(&db, 0, Some(5))?, vec![relevant, run_wide]);
  Ok(())
}

#[test]
fn rejects_a_missing_task_reference() -> Result<()> {
  let mut db = database();
  let transaction = db.transaction()?;
  let task_error = create(&transaction, Some(9), "observation").unwrap_err();
  transaction.rollback()?;

  assert_eq!(task_error.to_string(), "FOREIGN KEY constraint failed");
  Ok(())
}
