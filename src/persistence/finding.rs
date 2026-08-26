use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::domain::{Finding, FindingVerdict};

struct FindingRow {
  id: i64,
  task_id: i64,
  description: String,
  verdict: Option<String>,
  verdict_reason: Option<String>,
  fix_task_id: Option<i64>,
  created_at: i64,
  resolved_at: Option<i64>,
}

const SELECT: &str = "
  select id, task_id, description, verdict, verdict_reason,
         fix_task_id, created_at, resolved_at
  from findings
";

pub fn register(transaction: &Transaction<'_>, task_id: i64, description: &str) -> Result<Finding> {
  let created_at = Utc::now().timestamp_millis();
  let id = transaction.query_row(
    "
      insert into findings(task_id, description, created_at)
      values (?1, ?2, ?3) returning id
      ",
    params![task_id, description, created_at],
    |row| row.get(0),
  )?;
  let created_at = DateTime::from_timestamp_millis(created_at)
    .expect("current timestamp is inside the supported range");
  Finding::registered(id, task_id, description.to_owned(), created_at)
}

pub fn resolve(
  transaction: &Transaction<'_>,
  finding: &Finding,
  verdict: FindingVerdict,
  verdict_reason: &str,
  fix_task_id: Option<i64>,
) -> Result<Finding> {
  if finding.is_resolved() {
    bail!("finding {} is already resolved", finding.id());
  }
  let resolved_at = Utc::now().timestamp_millis();
  let candidate = Finding::from_record(
    finding.id(),
    finding.task_id(),
    finding.description().to_owned(),
    Some(verdict),
    Some(verdict_reason.to_owned()),
    fix_task_id,
    finding.created_at(),
    DateTime::from_timestamp_millis(resolved_at),
  )?;
  let changed = transaction.execute(
    "
      update findings
      set verdict=?1, verdict_reason=?2, fix_task_id=?3, resolved_at=?4
      where id=?5 and verdict is null
      ",
    params![
      verdict.as_str(),
      verdict_reason,
      fix_task_id,
      resolved_at,
      finding.id()
    ],
  )?;
  if changed != 1 {
    bail!("finding {} is already resolved", finding.id());
  }
  Ok(candidate)
}

pub fn get(transaction: &Transaction<'_>, id: i64) -> Result<Option<Finding>> {
  let sql = format!("{SELECT} where id=?");
  let row = transaction.query_row(&sql, [id], finding_row).optional()?;
  row.map(materialize).transpose()
}

pub fn unresolved(db: &Connection, task_id: Option<i64>) -> Result<Vec<Finding>> {
  let sql = format!("{SELECT} where verdict is null and (?1 is null or task_id=?1) order by id");
  let mut statement = db.prepare(&sql)?;
  load(statement.query_map([task_id], finding_row)?)
}

pub fn resolved(db: &Connection) -> Result<Vec<Finding>> {
  let sql = format!("{SELECT} where verdict is not null order by resolved_at, id");
  let mut statement = db.prepare(&sql)?;
  load(statement.query_map([], finding_row)?)
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
    resolved_at: row.get(7)?,
  })
}

fn load(rows: impl Iterator<Item = rusqlite::Result<FindingRow>>) -> Result<Vec<Finding>> {
  rows
    .map(|row| {
      let row = row?;
      materialize(row)
    })
    .collect()
}

fn materialize(row: FindingRow) -> Result<Finding> {
  let verdict = row
    .verdict
    .as_deref()
    .map(FindingVerdict::try_from)
    .transpose()?;
  let created_at = DateTime::from_timestamp_millis(row.created_at)
    .context("finding created_at is outside the supported range")?;
  let resolved_at = row
    .resolved_at
    .map(|timestamp| {
      DateTime::from_timestamp_millis(timestamp)
        .context("finding resolved_at is outside the supported range")
    })
    .transpose()?;
  Finding::from_record(
    row.id,
    row.task_id,
    row.description,
    verdict,
    row.verdict_reason,
    row.fix_task_id,
    created_at,
    resolved_at,
  )
}

#[cfg(test)]
mod tests {
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
    transaction.commit()?;

    assert_eq!(first.id(), 1);
    assert_eq!(second.id(), 2);
    assert!(!first.is_resolved());
    assert_eq!(unresolved(&db, None)?, vec![first.clone(), second]);
    assert_eq!(unresolved(&db, Some(5))?, vec![first]);
    Ok(())
  }

  #[test]
  fn leaves_registration_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    create_task(&db, 5)?;

    let transaction = db.transaction()?;
    register(&transaction, 5, "temporary defect")?;
    transaction.rollback()?;

    assert!(unresolved(&db, None)?.is_empty());
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
    transaction.commit()?;

    assert!(resolution.is_resolved());
    assert_eq!(resolution.verdict(), Some(FindingVerdict::Task));
    assert_eq!(unresolved(&db, None)?, Vec::new());
    assert_eq!(resolved(&db)?, vec![resolution.clone()]);
    assert_eq!(resolved(&db)?, vec![resolution.clone()]);

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
}
