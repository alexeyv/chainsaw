use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;

use super::{create, record_attempt, record_seen};
use crate::persistence::test_fixture::database;

/// The stored row as the supervisor would see it, with times in milliseconds.
fn stored_row(db: &Connection, id: i64) -> Result<String> {
  let row = db.query_row(
    "select session, text, sent_at, seen_at, attempts from prompts where id=?",
    [id],
    |row| {
      Ok(format!(
        "session={} text={} sent={} seen={:?} attempts={}",
        row.get::<_, String>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, i64>(2)?,
        row.get::<_, Option<i64>>(3)?,
        row.get::<_, i64>(4)?,
      ))
    },
  )?;
  Ok(row)
}

fn sent_at(db: &Connection, id: i64) -> Result<i64> {
  Ok(
    db.query_row("select sent_at from prompts where id=?", [id], |row| {
      row.get(0)
    })?,
  )
}

fn seen_at(db: &Connection, id: i64) -> Result<Option<i64>> {
  Ok(
    db.query_row("select seen_at from prompts where id=?", [id], |row| {
      row.get(0)
    })?,
  )
}

mod create {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let before = Utc::now().timestamp_millis();

    let transaction = db.transaction()?;
    let id = create(&transaction, "implementer-1", "continue")?;
    transaction.commit()?;

    let after = Utc::now().timestamp_millis();
    let sent = sent_at(&db, id)?;
    assert_eq!(id, 1);
    assert!((before..=after).contains(&sent));
    assert_eq!(
      stored_row(&db, id)?,
      format!("session=implementer-1 text=continue sent={sent} seen=None attempts=0")
    );
    Ok(())
  }

  #[test]
  fn should_number_prompts_in_order_of_creation() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let first = create(&transaction, "a", "one")?;
    let second = create(&transaction, "b", "two")?;

    assert_eq!((first, second), (1, 2));
    Ok(())
  }
}

mod record_attempt {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let id = create(&transaction, "implementer-1", "continue")?;

    record_attempt(&transaction, id)?;
    record_attempt(&transaction, id)?;
    transaction.commit()?;

    let sent = sent_at(&db, id)?;
    assert_eq!(
      stored_row(&db, id)?,
      format!("session=implementer-1 text=continue sent={sent} seen=None attempts=2")
    );
    Ok(())
  }
}

mod record_seen {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let id = create(&transaction, "implementer-1", "continue")?;
    let before = Utc::now().timestamp_millis();

    record_seen(&transaction, id)?;
    transaction.commit()?;

    let after = Utc::now().timestamp_millis();
    let seen = seen_at(&db, id)?.expect("seen_at is stamped");
    assert!((before..=after).contains(&seen));
    Ok(())
  }
}
