use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;

use super::{end, intervals, is_open, start};
use crate::persistence::test_fixture::{database, row_count};

/// Every stored wait as `started..ended`, with an open one ending in `open`.
fn stored_rows(db: &Connection) -> Result<String> {
  let mut statement = db.prepare("select started, ended from human_waits order by id")?;
  let rows = statement
    .query_map([], |row| {
      Ok(format!(
        "{}..{}",
        row.get::<_, i64>(0)?,
        row
          .get::<_, Option<i64>>(1)?
          .map_or_else(|| "open".to_owned(), |ended| ended.to_string())
      ))
    })?
    .collect::<rusqlite::Result<Vec<_>>>()?;
  Ok(rows.join("\n"))
}

mod start {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let before = Utc::now().timestamp_millis();

    let transaction = db.transaction()?;
    let opened = start(&transaction)?;
    transaction.commit()?;

    let after = Utc::now().timestamp_millis();
    let started: i64 = db.query_row("select started from human_waits", [], |row| row.get(0))?;
    assert!(opened);
    assert!((before..=after).contains(&started));
    assert_eq!(stored_rows(&db)?, format!("{started}..open"));
    Ok(())
  }

  #[test]
  fn should_leave_the_open_wait_alone_when_one_is_open() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    start(&transaction)?;

    let opened = start(&transaction)?;
    transaction.commit()?;

    assert!(!opened);
    assert_eq!(row_count(&db, "human_waits")?, 1);
    Ok(())
  }

  #[test]
  fn should_open_another_wait_when_the_last_one_ended() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    start(&transaction)?;
    end(&transaction)?;

    let opened = start(&transaction)?;
    transaction.commit()?;

    assert!(opened);
    assert_eq!(row_count(&db, "human_waits")?, 2);
    Ok(())
  }
}

mod end {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    start(&transaction)?;

    let closed = end(&transaction)?;
    transaction.commit()?;

    let (started, ended): (i64, Option<i64>) =
      db.query_row("select started, ended from human_waits", [], |row| {
        Ok((row.get(0)?, row.get(1)?))
      })?;
    assert!(closed);
    assert!(ended.is_some_and(|ended| ended >= started));
    Ok(())
  }

  #[test]
  fn should_do_nothing_when_no_wait_is_open() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let closed = end(&transaction)?;

    assert!(!closed);
    Ok(())
  }
}

mod is_open {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    start(&transaction)?;

    assert!(is_open(&transaction)?);
    Ok(())
  }

  #[test]
  fn should_be_false_when_every_wait_ended() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    start(&transaction)?;
    end(&transaction)?;

    assert!(!is_open(&transaction)?);
    Ok(())
  }

  #[test]
  fn should_be_false_when_no_wait_was_ever_opened() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    assert!(!is_open(&transaction)?);
    Ok(())
  }
}

mod intervals {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    db.execute_batch(
      "insert into human_waits(started, ended) values (100, 250);
       insert into human_waits(started) values (400);",
    )?;
    let transaction = db.transaction()?;

    let waits = intervals(&transaction)?;

    assert_eq!(waits, vec![(100, Some(250)), (400, None)]);
    Ok(())
  }

  #[test]
  fn should_be_empty_when_no_wait_was_ever_opened() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let waits = intervals(&transaction)?;

    assert_eq!(waits, Vec::new());
    Ok(())
  }
}
