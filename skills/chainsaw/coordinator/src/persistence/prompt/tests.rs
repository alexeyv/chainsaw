use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;

use super::{create, get, record_attempt, record_seen};
use crate::domain::test_helpers::{format_prompt, within};
use crate::persistence::test_fixture::{database, session_row};

/// The stored row as the supervisor would see it, with times in milliseconds.
fn stored_row(db: &Connection, id: i64) -> Result<String> {
  let row = db.query_row(
    "select session_id, text, sent_at, seen_at, attempts from prompts where id=?",
    [id],
    |row| {
      Ok(format!(
        "session_id={} text={} sent={} seen={:?} attempts={}",
        row.get::<_, i64>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, i64>(2)?,
        row.get::<_, Option<i64>>(3)?,
        row.get::<_, i64>(4)?,
      ))
    },
  )?;
  Ok(row)
}

mod create {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 5)?;
    let before = Utc::now();

    let transaction = db.transaction()?;
    let prompt = create(&transaction, 5, "continue")?;
    transaction.commit()?;

    let after = Utc::now();
    assert!(within(prompt.sent_at(), before, after));
    assert_eq!(
      format_prompt(&prompt),
      format!(
        "id: 1\nsession_id: 5\ntext: \"continue\"\nsent_at: {}\nseen_at: none\nattempts: 0",
        prompt
          .sent_at()
          .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
      )
    );
    assert_eq!(
      stored_row(&db, 1)?,
      format!(
        "session_id=5 text=continue sent={} seen=None attempts=0",
        prompt.sent_at().timestamp_millis()
      )
    );
    Ok(())
  }

  #[test]
  fn should_number_prompts_in_order_of_creation() -> Result<()> {
    let mut db = database();
    session_row(&db, 5)?;
    let transaction = db.transaction()?;

    let first = create(&transaction, 5, "one")?;
    let second = create(&transaction, 5, "two")?;

    assert_eq!((first.id(), second.id()), (1, 2));
    Ok(())
  }
}

mod get {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 5)?;
    let transaction = db.transaction()?;
    let created = create(&transaction, 5, "continue")?;

    let found = get(&transaction, created.id())?;

    assert_eq!(found, Some(created));
    Ok(())
  }

  #[test]
  fn should_be_none_when_no_prompt_has_the_id() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    assert_eq!(get(&transaction, 7)?, None);
    Ok(())
  }
}

mod record_attempt {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 5)?;
    let transaction = db.transaction()?;
    let created = create(&transaction, 5, "continue")?;

    record_attempt(&transaction, created.id())?;
    let prompt = record_attempt(&transaction, created.id())?;
    transaction.commit()?;

    assert_eq!(prompt.attempts(), 2);
    assert_eq!(get(&transaction_of(&mut db)?, 1)?, Some(prompt));
    Ok(())
  }

  #[test]
  fn should_fail_when_no_prompt_has_the_id() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let error = record_attempt(&transaction, 7).unwrap_err();

    assert_eq!(error.to_string(), "no prompt 7");
    Ok(())
  }
}

mod record_seen {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    session_row(&db, 5)?;
    let transaction = db.transaction()?;
    let created = create(&transaction, 5, "continue")?;
    let before = Utc::now();

    let prompt = record_seen(&transaction, created.id())?;
    transaction.commit()?;

    let after = Utc::now();
    let seen = prompt.seen_at().expect("seen_at is stamped");
    assert!(within(seen, before, after));
    assert_eq!(get(&transaction_of(&mut db)?, 1)?, Some(prompt));
    Ok(())
  }

  #[test]
  fn should_fail_when_no_prompt_has_the_id() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let error = record_seen(&transaction, 7).unwrap_err();

    assert_eq!(error.to_string(), "no prompt 7");
    Ok(())
  }
}

fn transaction_of(db: &mut Connection) -> Result<rusqlite::Transaction<'_>> {
  Ok(db.transaction()?)
}
