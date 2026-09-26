use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::{Store, initialize_schema};

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);

mod write_transaction {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let suffix = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
      "chainsaw-write-transaction-{}-{suffix}.db",
      std::process::id()
    ));
    let db = Connection::open(&path)?;
    db.busy_timeout(Duration::from_secs(2))?;
    db.execute_batch("create table counter(value int); insert into counter values(0);")?;
    let store = Store {
      run_dir: PathBuf::new(),
      logs_dir: PathBuf::new(),
      path: path.clone(),
      db,
    };
    let barrier = Arc::new(Barrier::new(2));
    let blocker_barrier = Arc::clone(&barrier);
    let blocker_path = path.clone();
    let blocker = thread::spawn(move || -> Result<()> {
      let db = Connection::open(blocker_path)?;
      let transaction = Transaction::new_unchecked(&db, TransactionBehavior::Immediate)?;
      blocker_barrier.wait();
      thread::sleep(Duration::from_millis(100));
      transaction.commit()?;
      Ok(())
    });
    barrier.wait();

    let transaction = store.write_transaction()?;
    let value =
      transaction.query_row("select value from counter", [], |row| row.get::<_, i64>(0))?;
    transaction.execute("update counter set value=?", [value + 1])?;
    transaction.commit()?;
    blocker.join().expect("writer thread panicked")?;
    let value = store
      .db
      .query_row("select value from counter", [], |row| row.get::<_, i64>(0))?;

    assert_eq!(value, 1);
    drop(store);
    let _ = fs::remove_file(path);
    Ok(())
  }
}

#[test]
fn creates_communication_storage_with_foreign_keys() -> Result<()> {
  let db = Connection::open_in_memory()?;

  initialize_schema(&db)?;

  let version = db.query_row("pragma user_version", [], |row| row.get::<_, i64>(0))?;
  let task_id_required = db.query_row(
    "select \"notnull\" from pragma_table_info('findings') where name='task_id'",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  let task_foreign_keys = db.query_row(
    "select count(*) from pragma_foreign_key_list('findings')
       where \"table\"='tasks' and \"from\" in ('task_id', 'fix_task_id')",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  let observation_foreign_keys = db.query_row(
    "select count(*) from pragma_foreign_key_list('observations')
       where \"table\"='tasks' and \"from\"='task_id'",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  let task_state_columns = db.query_row(
    "select count(*) from pragma_table_info('tasks') where name='state'",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  let legacy_finding_columns = db.query_row(
    "select count(*) from pragma_table_info('findings') where name='legacy_disposition'",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  let commentary_columns = db.query_row(
    "select count(*) from pragma_table_info('tasks')
       where name in ('commentary_requested_at', 'commentary_delivered_at')",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  let commentary_delivery_tables = db.query_row(
    "select count(*) from sqlite_schema where type='table' and name='commentary_deliveries'",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  let run_rows = db.query_row(
    "select count(*) from run where daemon_seen_at is null
       and stop_requested_at is null and state_read_at is null",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  assert_eq!(version, 1);
  assert_eq!(task_id_required, 1);
  assert_eq!(task_foreign_keys, 2);
  assert_eq!(observation_foreign_keys, 1);
  assert_eq!(task_state_columns, 0);
  assert_eq!(legacy_finding_columns, 0);
  assert_eq!(commentary_columns, 2);
  assert_eq!(commentary_delivery_tables, 0);
  assert_eq!(run_rows, 1);
  Ok(())
}

#[test]
fn refuses_a_database_from_another_schema_version() -> Result<()> {
  let db = Connection::open_in_memory()?;
  db.execute_batch("pragma user_version=2;")?;

  let error = initialize_schema(&db).unwrap_err();

  assert_eq!(
    error.to_string(),
    "database schema version 2 is unsupported; expected 1: remove the database and start a new run"
  );
  Ok(())
}

#[test]
fn initializes_one_database_concurrently() -> Result<()> {
  let suffix = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
  let path = std::env::temp_dir().join(format!(
    "chainsaw-schema-{}-{suffix}.db",
    std::process::id()
  ));
  let workers = 8;
  let barrier = Arc::new(Barrier::new(workers));
  let handles = (0..workers)
    .map(|_| {
      let barrier = Arc::clone(&barrier);
      let path = path.clone();
      thread::spawn(move || -> Result<()> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        barrier.wait();
        initialize_schema(&db)
      })
    })
    .collect::<Vec<_>>();

  let results = handles
    .into_iter()
    .map(|handle| handle.join())
    .collect::<Vec<_>>();
  let _ = fs::remove_file(&path);
  for result in results {
    result.expect("schema initialization worker panicked")?;
  }
  Ok(())
}
