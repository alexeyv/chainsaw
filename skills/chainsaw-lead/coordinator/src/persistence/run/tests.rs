use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;

use super::{clear_stop_request, get, record_daemon_seen, record_state_read, request_stop};
use crate::domain::test_helpers::{format_run, within};
use crate::persistence::test_fixture::database;

/// The stored row as the daemon would see it, with times in milliseconds.
fn stored_row(db: &Connection) -> Result<String> {
  let row = db.query_row(
    "select daemon_seen_at, stop_requested_at, state_read_at from run where id=1",
    [],
    |row| {
      Ok(format!(
        "seen={:?} stop={:?} read={:?}",
        row.get::<_, Option<i64>>(0)?,
        row.get::<_, Option<i64>>(1)?,
        row.get::<_, Option<i64>>(2)?,
      ))
    },
  )?;
  Ok(row)
}

fn delete_run_row(db: &Connection) -> Result<()> {
  db.execute("delete from run", [])?;
  Ok(())
}

mod get {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let run = get(&transaction)?;

    assert_eq!(
      format_run(&run),
      r#"daemon_seen_at: none
stop_requested_at: none
state_read_at: none
is_stopping: false"#
    );
    Ok(())
  }

  #[test]
  fn should_read_back_every_recorded_time() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let seen = record_daemon_seen(&transaction)?;
    let stopped = request_stop(&transaction)?;
    let read = record_state_read(&transaction)?;

    let run = get(&transaction)?;

    assert_eq!(run.daemon_seen_at(), seen.daemon_seen_at());
    assert_eq!(run.stop_requested_at(), stopped.stop_requested_at());
    assert_eq!(run.state_read_at(), read.state_read_at());
    assert_eq!(run, read);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_run_row_is_missing() -> Result<()> {
    let mut db = database();
    delete_run_row(&db)?;
    let transaction = db.transaction()?;

    let error = get(&transaction).unwrap_err();

    assert_eq!(error.to_string(), "run record is missing");
    Ok(())
  }
}

mod record_daemon_seen {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();

    let before = Utc::now();
    let transaction = db.transaction()?;
    let run = record_daemon_seen(&transaction)?;
    transaction.commit()?;
    let after = Utc::now();

    let seen = run.daemon_seen_at().unwrap();
    assert!(within(seen, before, after));
    assert_eq!(run.stop_requested_at(), None);
    assert_eq!(run.state_read_at(), None);
    assert_eq!(
      stored_row(&db)?,
      format!("seen=Some({}) stop=None read=None", seen.timestamp_millis())
    );
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    record_daemon_seen(&transaction)?;
    transaction.rollback()?;

    assert_eq!(stored_row(&db)?, "seen=None stop=None read=None");
    Ok(())
  }

  #[test]
  fn should_fail_when_the_run_row_is_missing() -> Result<()> {
    let mut db = database();
    delete_run_row(&db)?;
    let transaction = db.transaction()?;

    let error = record_daemon_seen(&transaction).unwrap_err();

    assert_eq!(error.to_string(), "run record is missing");
    Ok(())
  }
}

mod request_stop {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();

    let before = Utc::now();
    let transaction = db.transaction()?;
    let run = request_stop(&transaction)?;
    transaction.commit()?;
    let after = Utc::now();

    let requested = run.stop_requested_at().unwrap();
    assert!(within(requested, before, after));
    assert!(run.is_stopping());
    assert_eq!(
      stored_row(&db)?,
      format!(
        "seen=None stop=Some({}) read=None",
        requested.timestamp_millis()
      )
    );
    Ok(())
  }

  #[test]
  fn should_move_the_time_forward_when_requested_again() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let first = request_stop(&transaction)?;
    std::thread::sleep(std::time::Duration::from_millis(2));

    let second = request_stop(&transaction)?;

    assert!(second.stop_requested_at() > first.stop_requested_at());
    assert!(second.is_stopping());
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    request_stop(&transaction)?;
    transaction.rollback()?;

    assert_eq!(stored_row(&db)?, "seen=None stop=None read=None");
    Ok(())
  }

  #[test]
  fn should_fail_when_the_run_row_is_missing() -> Result<()> {
    let mut db = database();
    delete_run_row(&db)?;
    let transaction = db.transaction()?;

    let error = request_stop(&transaction).unwrap_err();

    assert_eq!(error.to_string(), "run record is missing");
    Ok(())
  }
}

mod clear_stop_request {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let seen = record_daemon_seen(&transaction)?;
    request_stop(&transaction)?;

    let run = clear_stop_request(&transaction)?;
    transaction.commit()?;

    assert!(!run.is_stopping());
    assert_eq!(run.daemon_seen_at(), seen.daemon_seen_at());
    assert_eq!(
      stored_row(&db)?,
      format!(
        "seen=Some({}) stop=None read=None",
        seen.daemon_seen_at().unwrap().timestamp_millis()
      )
    );
    Ok(())
  }

  #[test]
  fn should_leave_a_run_without_a_request_alone() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let run = clear_stop_request(&transaction)?;

    assert_eq!(
      format_run(&run),
      r#"daemon_seen_at: none
stop_requested_at: none
state_read_at: none
is_stopping: false"#
    );
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let stopped = request_stop(&transaction)?;
    transaction.commit()?;

    let transaction = db.transaction()?;
    clear_stop_request(&transaction)?;
    transaction.rollback()?;

    assert_eq!(
      stored_row(&db)?,
      format!(
        "seen=None stop=Some({}) read=None",
        stopped.stop_requested_at().unwrap().timestamp_millis()
      )
    );
    Ok(())
  }

  #[test]
  fn should_fail_when_the_run_row_is_missing() -> Result<()> {
    let mut db = database();
    delete_run_row(&db)?;
    let transaction = db.transaction()?;

    let error = clear_stop_request(&transaction).unwrap_err();

    assert_eq!(error.to_string(), "run record is missing");
    Ok(())
  }
}

mod record_state_read {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();

    let before = Utc::now();
    let transaction = db.transaction()?;
    let run = record_state_read(&transaction)?;
    transaction.commit()?;
    let after = Utc::now();

    let read = run.state_read_at().unwrap();
    assert!(within(read, before, after));
    assert_eq!(run.daemon_seen_at(), None);
    assert_eq!(run.stop_requested_at(), None);
    assert_eq!(
      stored_row(&db)?,
      format!("seen=None stop=None read=Some({})", read.timestamp_millis())
    );
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    record_state_read(&transaction)?;
    transaction.rollback()?;

    assert_eq!(stored_row(&db)?, "seen=None stop=None read=None");
    Ok(())
  }

  #[test]
  fn should_fail_when_the_run_row_is_missing() -> Result<()> {
    let mut db = database();
    delete_run_row(&db)?;
    let transaction = db.transaction()?;

    let error = record_state_read(&transaction).unwrap_err();

    assert_eq!(error.to_string(), "run record is missing");
    Ok(())
  }
}
