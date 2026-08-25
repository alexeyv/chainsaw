use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Transaction, params};

use crate::domain::{Finding, FindingVerdict};

struct FindingRow {
  id: i64,
  task_id: i64,
  description: String,
  verdict: String,
  verdict_reason: String,
  fix_task_id: Option<i64>,
  created_at: i64,
}

pub fn create(
  transaction: &Transaction<'_>,
  task_id: i64,
  description: &str,
  verdict: FindingVerdict,
  verdict_reason: &str,
  fix_task_id: Option<i64>,
) -> Result<Finding> {
  insert(
    transaction,
    task_id,
    description,
    verdict,
    verdict_reason,
    fix_task_id,
  )
}

pub(super) fn insert(
  transaction: &Transaction<'_>,
  task_id: i64,
  description: &str,
  verdict: FindingVerdict,
  verdict_reason: &str,
  fix_task_id: Option<i64>,
) -> Result<Finding> {
  let created_at = Utc::now().timestamp_millis();
  let id = transaction.query_row(
    "
      insert into findings(
        task_id, description, verdict, verdict_reason, fix_task_id, created_at
      ) values (?1, ?2, ?3, ?4, ?5, ?6)
      returning id
      ",
    params![
      task_id,
      description,
      verdict.as_str(),
      verdict_reason,
      fix_task_id,
      created_at,
    ],
    |row| row.get(0),
  )?;
  let created_at = DateTime::from_timestamp_millis(created_at)
    .expect("current timestamp is inside the supported range");
  let finding = Finding::new(
    id,
    task_id,
    description.to_owned(),
    verdict,
    verdict_reason.to_owned(),
    fix_task_id,
    created_at,
  )?;
  Ok(finding)
}

pub(super) fn for_task(transaction: &Transaction<'_>, task_id: i64) -> Result<Vec<Finding>> {
  let mut statement = transaction.prepare(
    "
      select id, task_id, description, verdict, verdict_reason, fix_task_id, created_at
      from findings where task_id=? order by id
      ",
  )?;
  load(statement.query_map([task_id], finding_row)?)
}

pub fn all(db: &Connection) -> Result<Vec<Finding>> {
  let mut statement = db.prepare(
    "
      select id, task_id, description, verdict, verdict_reason, fix_task_id, created_at
      from findings order by id
      ",
  )?;
  let rows = statement.query_map([], finding_row)?;
  load(rows)
}

fn finding_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FindingRow> {
  Ok(FindingRow {
    id: row.get(0)?,
    task_id: row.get(1)?,
    description: row.get(2)?,
    verdict: row.get(3)?,
    verdict_reason: row.get(4)?,
    fix_task_id: row.get(5)?,
    created_at: row.get(6)?,
  })
}

fn load(rows: impl Iterator<Item = rusqlite::Result<FindingRow>>) -> Result<Vec<Finding>> {
  rows
    .map(|row| {
      let row = row?;
      let verdict = FindingVerdict::try_from(row.verdict.as_str())?;
      let created_at = DateTime::from_timestamp_millis(row.created_at)
        .context("finding created_at is outside the supported range")?;
      Finding::new(
        row.id,
        row.task_id,
        row.description,
        verdict,
        row.verdict_reason,
        row.fix_task_id,
        created_at,
      )
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use anyhow::Result;
  use chrono::Utc;
  use rusqlite::Connection;

  use super::{all, create};
  use crate::domain::FindingVerdict;
  use crate::persistence::test_fixture::database;

  fn create_task(db: &Connection, id: i64) -> Result<()> {
    db.execute("insert into tasks(id) values(?)", [id])?;
    Ok(())
  }

  #[test]
  fn creates_a_valid_finding() -> Result<()> {
    let mut db = database();
    create_task(&db, 5)?;
    create_task(&db, 7)?;

    let before = Utc::now().timestamp_millis();
    let transaction = db.transaction()?;
    let finding = create(
      &transaction,
      5,
      "verification can accept the wrong commit",
      FindingVerdict::Task,
      "the check trusts an ambiguous log entry",
      Some(7),
    )?;
    transaction.commit()?;
    let after = Utc::now().timestamp_millis();

    let stored = db.query_row(
      "
        select task_id, description, verdict, verdict_reason, fix_task_id, created_at
        from findings where id=?
        ",
      [finding.id()],
      |row| {
        Ok((
          row.get::<_, i64>(0)?,
          row.get::<_, String>(1)?,
          row.get::<_, String>(2)?,
          row.get::<_, String>(3)?,
          row.get::<_, Option<i64>>(4)?,
          row.get::<_, i64>(5)?,
        ))
      },
    )?;
    assert!(finding.created_at().timestamp_millis() >= before);
    assert!(finding.created_at().timestamp_millis() <= after);
    assert_eq!(
      stored,
      (
        5,
        "verification can accept the wrong commit".to_owned(),
        "task".to_owned(),
        "the check trusts an ambiguous log entry".to_owned(),
        Some(7),
        finding.created_at().timestamp_millis(),
      )
    );
    Ok(())
  }

  #[test]
  fn assigns_autoincrementing_ids() -> Result<()> {
    let mut db = database();
    create_task(&db, 1)?;

    let transaction = db.transaction()?;
    let first = create(
      &transaction,
      1,
      "first",
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    let second = create(
      &transaction,
      1,
      "second",
      FindingVerdict::Dropped,
      "already covered",
      None,
    )?;
    transaction.commit()?;

    assert_eq!(first.id(), 1);
    assert_eq!(second.id(), 2);
    Ok(())
  }

  #[test]
  fn loads_all_findings_in_identity_order() -> Result<()> {
    let mut db = database();
    create_task(&db, 1)?;

    let transaction = db.transaction()?;
    let first = create(
      &transaction,
      1,
      "first",
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    let second = create(
      &transaction,
      1,
      "second",
      FindingVerdict::Dropped,
      "already covered",
      None,
    )?;
    transaction.commit()?;

    assert_eq!(all(&db)?, vec![first, second]);
    Ok(())
  }

  #[test]
  fn leaves_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    create_task(&db, 1)?;

    let transaction = db.transaction()?;
    create(
      &transaction,
      1,
      "a defect",
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    transaction.rollback()?;

    assert!(all(&db)?.is_empty());
    Ok(())
  }

  #[test]
  fn rejects_invalid_data_before_commit() -> Result<()> {
    let mut db = database();
    create_task(&db, 1)?;

    let transaction = db.transaction()?;
    let error = create(
      &transaction,
      1,
      " ",
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )
    .unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "description cannot be blank");
    assert!(all(&db)?.is_empty());
    Ok(())
  }

  #[test]
  fn rejects_a_missing_fix_task() -> Result<()> {
    let mut db = database();
    create_task(&db, 1)?;

    let transaction = db.transaction()?;
    let error = create(
      &transaction,
      1,
      "a defect",
      FindingVerdict::Task,
      "worth fixing",
      Some(7),
    )
    .unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    assert!(all(&db)?.is_empty());
    Ok(())
  }

  #[test]
  fn rejects_a_missing_task() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(
      &transaction,
      7,
      "a defect",
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )
    .unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    assert!(all(&db)?.is_empty());
    Ok(())
  }
}
