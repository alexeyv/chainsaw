use anyhow::Result;
use rusqlite::Connection;

use super::{get, register, resolve, resolved, unresolved};
use crate::domain::FindingVerdict;
use crate::persistence::test_fixture::database;

fn create_task(db: &Connection, id: i64) -> Result<()> {
  db.execute("insert into tasks(id) values(?)", [id])?;
  Ok(())
}

#[test]
fn registers_stable_unresolved_findings_and_filters_them_by_task() -> Result<()> {
  let mut db = database();
  create_task(&db, 5)?;
  create_task(&db, 6)?;

  let transaction = db.transaction()?;
  let first = register(&transaction, 5, "first defect")?;
  let second = register(&transaction, 6, "second defect")?;

  assert_eq!(first.id(), 1);
  assert_eq!(second.id(), 2);
  assert!(!first.is_resolved());
  assert_eq!(unresolved(&transaction, None)?, vec![first.clone(), second]);
  assert_eq!(unresolved(&transaction, Some(5))?, vec![first]);
  transaction.commit()?;
  Ok(())
}

#[test]
fn leaves_registration_commit_and_rollback_to_the_caller() -> Result<()> {
  let mut db = database();
  create_task(&db, 5)?;

  let transaction = db.transaction()?;
  register(&transaction, 5, "temporary defect")?;
  transaction.rollback()?;

  let transaction = db.transaction()?;
  assert!(unresolved(&transaction, None)?.is_empty());
  transaction.commit()?;
  Ok(())
}

#[test]
fn resolves_once_and_exposes_the_resolution_run_wide() -> Result<()> {
  let mut db = database();
  create_task(&db, 5)?;
  create_task(&db, 6)?;

  let transaction = db.transaction()?;
  let finding = register(&transaction, 5, "a defect")?;
  let resolution = resolve(
    &transaction,
    &finding,
    FindingVerdict::Task,
    "fix it",
    Some(6),
  )?;

  assert!(resolution.is_resolved());
  assert_eq!(resolution.verdict(), Some(FindingVerdict::Task));
  assert_eq!(unresolved(&transaction, None)?, Vec::new());
  assert_eq!(resolved(&transaction)?, vec![resolution.clone()]);
  assert_eq!(resolved(&transaction)?, vec![resolution.clone()]);
  transaction.commit()?;

  let transaction = db.transaction()?;
  let stored = get(&transaction, resolution.id())?.expect("stored finding");
  let error = resolve(
    &transaction,
    &stored,
    FindingVerdict::Dropped,
    "changed mind",
    None,
  )
  .unwrap_err();
  transaction.rollback()?;
  assert_eq!(error.to_string(), "finding 1 is already resolved");
  Ok(())
}

#[test]
fn validates_resolution_shape_before_updating_the_finding() -> Result<()> {
  let mut db = database();
  create_task(&db, 5)?;
  create_task(&db, 6)?;
  let transaction = db.transaction()?;
  let finding = register(&transaction, 5, "a defect")?;

  let missing_fix =
    resolve(&transaction, &finding, FindingVerdict::Task, "fix it", None).unwrap_err();
  let unwanted_fix = resolve(
    &transaction,
    &finding,
    FindingVerdict::Dropped,
    "not actionable",
    Some(6),
  )
  .unwrap_err();
  transaction.rollback()?;

  assert_eq!(
    missing_fix.to_string(),
    "task verdict requires a fix_task_id"
  );
  assert_eq!(
    unwanted_fix.to_string(),
    "dropped verdict cannot have a fix_task_id"
  );
  Ok(())
}
