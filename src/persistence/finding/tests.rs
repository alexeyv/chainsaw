use anyhow::Result;
use chrono::{DateTime, Utc};

fn millisecond_floor(time: DateTime<Utc>) -> DateTime<Utc> {
  DateTime::from_timestamp_millis(time.timestamp_millis()).unwrap()
}

use super::{get, register, resolve, resolved, unresolved};
use crate::domain::test_helpers::format_finding;
use crate::domain::{Finding, FindingVerdict};
use crate::persistence::test_fixture::{database, row_count, task_row};

fn format_findings(findings: &[Finding]) -> String {
  findings
    .iter()
    .map(format_finding)
    .collect::<Vec<_>>()
    .join("\n\n")
}

mod register {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;

    let before = millisecond_floor(Utc::now());
    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "verification can accept the wrong commit")?;
    transaction.commit()?;
    let after = Utc::now();

    let stored = db.query_row(
      "
        select task_id, description, verdict, verdict_reason, fix_task_id,
               created_at, resolved_at
        from findings where id=?
        ",
      [finding.id()],
      |row| {
        Ok((
          row.get::<_, i64>(0)?,
          row.get::<_, String>(1)?,
          row.get::<_, Option<String>>(2)?,
          row.get::<_, Option<String>>(3)?,
          row.get::<_, Option<i64>>(4)?,
          row.get::<_, i64>(5)?,
          row.get::<_, Option<i64>>(6)?,
        ))
      },
    )?;
    assert_eq!(finding.id(), 1);
    assert_eq!(finding.task_id(), 5);
    assert_eq!(
      finding.description(),
      "verification can accept the wrong commit"
    );
    assert!(!finding.is_resolved());
    assert!(finding.created_at() >= before);
    assert!(finding.created_at() <= after);
    assert_eq!(
      stored,
      (
        5,
        "verification can accept the wrong commit".to_owned(),
        None,
        None,
        None,
        finding.created_at().timestamp_millis(),
        None,
      )
    );
    Ok(())
  }

  #[test]
  fn should_assign_increasing_ids() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;

    let transaction = db.transaction()?;
    let first = register(&transaction, 5, "first defect")?;
    let second = register(&transaction, 5, "second defect")?;
    transaction.commit()?;

    assert_eq!((first.id(), second.id()), (1, 2));
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;

    let transaction = db.transaction()?;
    register(&transaction, 5, "temporary defect")?;
    transaction.rollback()?;

    assert_eq!(row_count(&db, "findings")?, 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_does_not_exist() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = register(&transaction, 5, "a defect").unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    Ok(())
  }

  #[test]
  fn should_fail_when_the_description_is_blank() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;

    let transaction = db.transaction()?;
    let error = register(&transaction, 5, " ").unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "description cannot be blank");
    Ok(())
  }
}

mod resolve {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    task_row(&db, 6)?;

    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;
    let before = millisecond_floor(Utc::now());
    let resolution = resolve(
      &transaction,
      &finding,
      FindingVerdict::Task,
      "worth fixing",
      Some(6),
    )?;
    let after = Utc::now();
    transaction.commit()?;

    let stored = db.query_row(
      "select verdict, verdict_reason, fix_task_id, resolved_at from findings where id=?",
      [finding.id()],
      |row| {
        Ok((
          row.get::<_, Option<String>>(0)?,
          row.get::<_, Option<String>>(1)?,
          row.get::<_, Option<i64>>(2)?,
          row.get::<_, Option<i64>>(3)?,
        ))
      },
    )?;
    let resolved_at = resolution.resolved_at().expect("resolution time");
    assert!(resolved_at >= before);
    assert!(resolved_at <= after);
    assert_eq!(
      format_finding(&resolution),
      format!(
        r#"id: 1
task_id: 5
description: "a defect"
verdict: task
verdict_reason: "worth fixing"
fix_task_id: 6
created_at: {}
resolved_at: {}
is_resolved: true"#,
        finding
          .created_at()
          .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        resolved_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
      )
    );
    assert_eq!(
      stored,
      (
        Some("task".to_owned()),
        Some("worth fixing".to_owned()),
        Some(6),
        Some(resolved_at.timestamp_millis()),
      )
    );
    Ok(())
  }

  #[test]
  fn should_drop_a_finding_without_a_fix_task() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;

    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;
    let resolution = resolve(
      &transaction,
      &finding,
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    transaction.commit()?;

    assert_eq!(resolution.verdict(), Some(FindingVerdict::Dropped));
    assert_eq!(resolution.verdict_reason(), Some("not actionable"));
    assert_eq!(resolution.fix_task_id(), None);
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    resolve(
      &transaction,
      &finding,
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    transaction.rollback()?;

    let transaction = db.transaction()?;
    let stored = get(&transaction, finding.id())?.expect("stored finding");
    transaction.commit()?;
    assert!(!stored.is_resolved());
    Ok(())
  }

  #[test]
  fn should_fail_when_the_finding_is_already_resolved() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;
    let resolution = resolve(
      &transaction,
      &finding,
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    let error = resolve(
      &transaction,
      &resolution,
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
  fn should_fail_when_a_stale_unresolved_finding_was_resolved_elsewhere() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    let stale = register(&transaction, 5, "a defect")?;
    resolve(
      &transaction,
      &stale,
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    let error = resolve(
      &transaction,
      &stale,
      FindingVerdict::Dropped,
      "changed mind",
      None,
    )
    .unwrap_err();
    let stored = get(&transaction, stale.id())?.expect("stored finding");
    transaction.rollback()?;

    assert_eq!(error.to_string(), "finding 1 is already resolved");
    assert_eq!(stored.verdict_reason(), Some("not actionable"));
    Ok(())
  }

  #[test]
  fn should_fail_when_a_task_verdict_has_no_fix_task() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;

    let error = resolve(&transaction, &finding, FindingVerdict::Task, "fix it", None).unwrap_err();
    let stored = get(&transaction, finding.id())?.expect("stored finding");
    transaction.rollback()?;

    assert_eq!(error.to_string(), "task verdict requires a fix_task_id");
    assert!(!stored.is_resolved());
    Ok(())
  }

  #[test]
  fn should_fail_when_a_dropped_verdict_has_a_fix_task() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    task_row(&db, 6)?;
    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;

    let error = resolve(
      &transaction,
      &finding,
      FindingVerdict::Dropped,
      "not actionable",
      Some(6),
    )
    .unwrap_err();
    let stored = get(&transaction, finding.id())?.expect("stored finding");
    transaction.rollback()?;

    assert_eq!(
      error.to_string(),
      "dropped verdict cannot have a fix_task_id"
    );
    assert!(!stored.is_resolved());
    Ok(())
  }

  #[test]
  fn should_fail_when_the_fix_task_does_not_exist() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;

    let error = resolve(
      &transaction,
      &finding,
      FindingVerdict::Task,
      "fix it",
      Some(99),
    )
    .unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    Ok(())
  }
}

mod get {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    task_row(&db, 6)?;
    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;
    let resolution = resolve(
      &transaction,
      &finding,
      FindingVerdict::Task,
      "worth fixing",
      Some(6),
    )?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    let stored = get(&transaction, finding.id())?.expect("stored finding");
    transaction.commit()?;

    assert_eq!(format_finding(&stored), format_finding(&resolution));
    Ok(())
  }

  #[test]
  fn should_read_an_unresolved_finding() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;
    let stored = get(&transaction, finding.id())?.expect("stored finding");
    transaction.commit()?;

    assert_eq!(format_finding(&stored), format_finding(&finding));
    Ok(())
  }

  #[test]
  fn should_return_none_when_the_finding_does_not_exist() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    assert_eq!(get(&transaction, 1)?, None);
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_fail_when_the_stored_verdict_is_unknown() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;
    transaction.execute(
      "update findings set verdict='maybe', verdict_reason='?', resolved_at=1 where id=?",
      [finding.id()],
    )?;

    let error = get(&transaction, finding.id()).unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "unknown finding verdict \"maybe\"");
    Ok(())
  }
}

mod unresolved {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    task_row(&db, 6)?;
    let transaction = db.transaction()?;
    let first = register(&transaction, 5, "first defect")?;
    let second = register(&transaction, 6, "second defect")?;
    let third = register(&transaction, 5, "third defect")?;
    resolve(
      &transaction,
      &second,
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;

    assert_eq!(
      format_findings(&unresolved(&transaction, None)?),
      format_findings(&[first, third])
    );
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_only_include_the_given_task_when_filtering_by_task() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    task_row(&db, 6)?;
    let transaction = db.transaction()?;
    let first = register(&transaction, 5, "first defect")?;
    register(&transaction, 6, "second defect")?;
    let third = register(&transaction, 5, "third defect")?;

    assert_eq!(
      format_findings(&unresolved(&transaction, Some(5))?),
      format_findings(&[first, third])
    );
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_return_nothing_when_every_finding_is_resolved() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    let finding = register(&transaction, 5, "a defect")?;
    resolve(
      &transaction,
      &finding,
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;

    assert_eq!(unresolved(&transaction, None)?, Vec::new());
    assert_eq!(unresolved(&transaction, Some(5))?, Vec::new());
    transaction.commit()?;
    Ok(())
  }
}

mod resolved {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    task_row(&db, 6)?;
    let transaction = db.transaction()?;
    let first = register(&transaction, 5, "first defect")?;
    let second = register(&transaction, 6, "second defect")?;
    register(&transaction, 5, "third defect")?;
    let first = resolve(
      &transaction,
      &first,
      FindingVerdict::Task,
      "worth fixing",
      Some(6),
    )?;
    let second = resolve(
      &transaction,
      &second,
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;

    assert_eq!(
      format_findings(&resolved(&transaction)?),
      format_findings(&[first, second])
    );
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_order_by_resolution_time_before_id() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    let first = register(&transaction, 5, "first defect")?;
    let second = register(&transaction, 5, "second defect")?;
    let first = resolve(
      &transaction,
      &first,
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    let second = resolve(
      &transaction,
      &second,
      FindingVerdict::Dropped,
      "not actionable",
      None,
    )?;
    transaction.execute(
      "update findings set resolved_at=? where id=?",
      [
        first.resolved_at().unwrap().timestamp_millis() - 1_000,
        second.id(),
      ],
    )?;
    let second = get(&transaction, second.id())?.expect("second finding");

    assert_eq!(
      format_findings(&resolved(&transaction)?),
      format_findings(&[second, first])
    );
    transaction.commit()?;
    Ok(())
  }

  #[test]
  fn should_return_nothing_when_no_finding_is_resolved() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    register(&transaction, 5, "a defect")?;

    assert_eq!(resolved(&transaction)?, Vec::new());
    transaction.commit()?;
    Ok(())
  }
}
