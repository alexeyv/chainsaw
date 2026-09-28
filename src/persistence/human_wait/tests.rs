use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;

use super::{all, end, open, start};
use crate::domain::HumanWait;
use crate::domain::test_helpers::{format_human_wait, within};
use crate::persistence::test_fixture::{database, row_count};

fn format_human_waits(waits: &[HumanWait]) -> String {
  waits
    .iter()
    .map(format_human_wait)
    .collect::<Vec<_>>()
    .join("\n\n")
}

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
    let before = Utc::now();

    let transaction = db.transaction()?;
    let wait = start(&transaction)?;
    transaction.commit()?;

    let after = Utc::now();
    assert_eq!(wait.id(), 1);
    assert!(wait.is_open());
    assert!(within(wait.started(), before, after));
    assert_eq!(
      stored_rows(&db)?,
      format!("{}..open", wait.started().timestamp_millis())
    );
    Ok(())
  }

  #[test]
  fn should_return_the_open_wait_when_one_is_open() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let first = start(&transaction)?;

    let again = start(&transaction)?;
    transaction.commit()?;

    assert_eq!(again, first);
    assert_eq!(row_count(&db, "human_waits")?, 1);
    Ok(())
  }

  #[test]
  fn should_open_another_wait_when_the_last_one_ended() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    start(&transaction)?;
    end(&transaction)?;

    let wait = start(&transaction)?;
    transaction.commit()?;

    assert_eq!(wait.id(), 2);
    assert!(wait.is_open());
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
    let opened = start(&transaction)?;

    let closed = end(&transaction)?.expect("the open wait is closed");
    transaction.commit()?;

    let ended = closed.ended().expect("ended is stamped");
    assert_eq!(closed.id(), opened.id());
    assert_eq!(closed.started(), opened.started());
    assert!(ended >= opened.started());
    assert_eq!(
      stored_rows(&db)?,
      format!(
        "{}..{}",
        opened.started().timestamp_millis(),
        ended.timestamp_millis()
      )
    );
    Ok(())
  }

  #[test]
  fn should_be_none_when_no_wait_is_open() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    assert_eq!(end(&transaction)?, None);
    Ok(())
  }
}

mod open {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let started = start(&transaction)?;

    assert_eq!(open(&transaction)?, Some(started));
    Ok(())
  }

  #[test]
  fn should_be_none_when_every_wait_ended() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    start(&transaction)?;
    end(&transaction)?;

    assert_eq!(open(&transaction)?, None);
    Ok(())
  }

  #[test]
  fn should_be_none_when_no_wait_was_ever_opened() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    assert_eq!(open(&transaction)?, None);
    Ok(())
  }
}

mod all {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    db.execute_batch(
      "insert into human_waits(started, ended) values (1700000000000, 1700000300000);
       insert into human_waits(started) values (1700000400000);",
    )?;
    let transaction = db.transaction()?;

    let waits = all(&transaction)?;

    assert_eq!(
      format_human_waits(&waits),
      r#"id: 1
started: 2023-11-14T22:13:20Z
ended: 2023-11-14T22:18:20Z
is_open: false

id: 2
started: 2023-11-14T22:20:00Z
ended: none
is_open: true"#
    );
    Ok(())
  }

  #[test]
  fn should_be_empty_when_no_wait_was_ever_opened() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    assert_eq!(all(&transaction)?, Vec::new());
    Ok(())
  }
}
