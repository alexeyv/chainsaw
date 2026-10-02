use anyhow::Result;
use chrono::{DateTime, Utc};

use super::{create, recent};
use crate::domain::{RunEvent, RunEventKind};
use crate::persistence::test_fixture::{database, row_count};

fn millisecond_floor(time: DateTime<Utc>) -> DateTime<Utc> {
  DateTime::from_timestamp_millis(time.timestamp_millis()).unwrap()
}

fn format_run_events(events: &[RunEvent]) -> String {
  events
    .iter()
    .map(|event| format!("{} {} {}", event.id(), event.kind(), event.detail()))
    .collect::<Vec<_>>()
    .join("\n")
}

mod create {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();

    let before = millisecond_floor(Utc::now());
    let transaction = db.transaction()?;
    let event = create(&transaction, RunEventKind::Kick, "implementer-1")?;
    transaction.commit()?;
    let after = Utc::now();

    let stored = db.query_row(
      "select kind, detail, created_at from run_events where id=?",
      [event.id()],
      |row| {
        Ok((
          row.get::<_, String>(0)?,
          row.get::<_, String>(1)?,
          row.get::<_, i64>(2)?,
        ))
      },
    )?;
    assert_eq!(event.id(), 1);
    assert_eq!(event.kind(), RunEventKind::Kick);
    assert_eq!(event.detail(), "implementer-1");
    assert!(event.created_at() >= before);
    assert!(event.created_at() <= after);
    assert_eq!(
      stored,
      (
        "kick".to_owned(),
        "implementer-1".to_owned(),
        event.created_at().timestamp_millis()
      )
    );
    Ok(())
  }

  #[test]
  fn should_assign_increasing_ids() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let first = create(&transaction, RunEventKind::DaemonStart, "pid 1")?;
    let second = create(&transaction, RunEventKind::DaemonExit, "pid 1")?;
    transaction.commit()?;

    assert_eq!((first.id(), second.id()), (1, 2));
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    create(&transaction, RunEventKind::Kick, "implementer-1")?;
    transaction.rollback()?;

    assert_eq!(row_count(&db, "run_events")?, 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_detail_is_blank() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(&transaction, RunEventKind::Kick, " ").unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "detail cannot be blank");
    Ok(())
  }
}

mod recent {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    create(&transaction, RunEventKind::DaemonStart, "pid 1")?;
    create(&transaction, RunEventKind::Kick, "implementer-1")?;
    create(&transaction, RunEventKind::Launch, "implementer-2")?;
    create(&transaction, RunEventKind::Compact, "commentator at 60000")?;
    create(&transaction, RunEventKind::Kick, "implementer-2")?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    let events = recent(
      &transaction,
      &[RunEventKind::Kick, RunEventKind::Compact],
      2,
    )?;
    transaction.commit()?;

    assert_eq!(
      format_run_events(&events),
      "5 kick implementer-2\n4 compact commentator at 60000"
    );
    Ok(())
  }

  #[test]
  fn should_return_everything_of_the_kinds_when_fewer_than_the_limit_exist() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    create(&transaction, RunEventKind::DaemonStart, "pid 1")?;
    create(&transaction, RunEventKind::Kick, "implementer-1")?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    let events = recent(&transaction, &[RunEventKind::Kick], 5)?;
    transaction.commit()?;

    assert_eq!(format_run_events(&events), "2 kick implementer-1");
    Ok(())
  }

  #[test]
  fn should_fail_when_the_stored_created_at_is_out_of_range() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let event = create(&transaction, RunEventKind::Kick, "implementer-1")?;
    transaction.execute(
      "update run_events set created_at=? where id=?",
      [i64::MAX, event.id()],
    )?;

    let error = recent(&transaction, &[RunEventKind::Kick], 5).unwrap_err();
    transaction.rollback()?;

    assert_eq!(
      error.to_string(),
      "run event created_at is outside the supported range"
    );
    Ok(())
  }

  #[test]
  fn should_return_nothing_when_no_kinds_are_asked_for() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    create(&transaction, RunEventKind::Kick, "implementer-1")?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    let events = recent(&transaction, &[], 5)?;
    transaction.commit()?;

    assert_eq!(events, Vec::<RunEvent>::new());
    Ok(())
  }
}
