use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use rusqlite::{Connection, MAIN_DB, TransactionState};

use super::{Store, initialize_schema};

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);

mod write_transaction {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let store = counter_store()?;

    let transaction = store.write_transaction()?;

    // A deferred transaction would still have no lock before its first read.
    assert_eq!(
      transaction.transaction_state(Some(MAIN_DB))?,
      TransactionState::Write
    );
    Ok(())
  }
}

fn counter_store() -> Result<Store> {
  let db = Connection::open_in_memory()?;
  db.execute_batch("create table counter(value int); insert into counter values(0);")?;
  Ok(Store {
    path: ":memory:".into(),
    db,
  })
}

fn counter(store: &Store) -> Result<i64> {
  Ok(
    store
      .db
      .query_row("select value from counter", [], |row| row.get(0))?,
  )
}

mod read {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let store = counter_store()?;

    let value = store.read(|transaction| {
      Ok(transaction.query_row("select value from counter", [], |row| row.get::<_, i64>(0))?)
    })?;

    assert_eq!(value, 0);
    Ok(())
  }

  #[test]
  fn should_fail_with_the_work_error_when_the_work_fails() -> Result<()> {
    let store = counter_store()?;

    let error = store
      .read(|_| -> Result<()> { anyhow::bail!("no such row") })
      .unwrap_err();

    assert_eq!(error.to_string(), "no such row");
    Ok(())
  }
}

mod write {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let store = counter_store()?;

    let value = store.write(|transaction| {
      transaction.execute("update counter set value=value+1", [])?;
      Ok(transaction.query_row("select value from counter", [], |row| row.get::<_, i64>(0))?)
    })?;

    assert_eq!(value, 1);
    assert_eq!(counter(&store)?, 1);
    Ok(())
  }

  #[test]
  fn should_roll_back_when_the_work_fails() -> Result<()> {
    let store = counter_store()?;

    let error = store
      .write(|transaction| -> Result<()> {
        transaction.execute("update counter set value=value+1", [])?;
        anyhow::bail!("changed my mind")
      })
      .unwrap_err();

    assert_eq!(error.to_string(), "changed my mind");
    assert_eq!(counter(&store)?, 0);
    Ok(())
  }
}

#[test]
fn creates_communication_storage_with_foreign_keys() -> Result<()> {
  let db = Connection::open_in_memory()?;

  initialize_schema(&db)?;

  let version = db.query_row("pragma user_version", [], |row| row.get::<_, i64>(0))?;
  let launch_columns = db.query_row(
    "select count(*) from pragma_table_info('sessions') where name in ('pane_id', 'role_prompt')",
    [],
    |row| row.get::<_, i64>(0),
  )?;
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
  let run_event_columns = db.query_row(
    "select count(*) from pragma_table_info('run_events')
       where name in ('id', 'kind', 'detail', 'created_at') and \"notnull\"=1",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  let legacy_event_tables = db.query_row(
    "select count(*) from sqlite_schema where type='table' and name='events'",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  let run_rows = db.query_row(
    "select count(*) from run where daemon_seen_at is null
       and stop_requested_at is null and state_read_at is null
       and observation_cursor=0 and daemon_pid is null",
    [],
    |row| row.get::<_, i64>(0),
  )?;
  assert_eq!(version, 2);
  assert_eq!(launch_columns, 2);
  assert_eq!(task_id_required, 1);
  assert_eq!(task_foreign_keys, 2);
  assert_eq!(observation_foreign_keys, 1);
  assert_eq!(task_state_columns, 0);
  assert_eq!(legacy_finding_columns, 0);
  assert_eq!(commentary_columns, 2);
  assert_eq!(commentary_delivery_tables, 0);
  assert_eq!(run_event_columns, 3);
  assert_eq!(legacy_event_tables, 0);
  assert_eq!(run_rows, 1);
  Ok(())
}

#[test]
fn refuses_a_database_from_another_schema_version() -> Result<()> {
  let db = Connection::open_in_memory()?;
  db.execute_batch("pragma user_version=3;")?;

  let error = initialize_schema(&db).unwrap_err();

  assert_eq!(
    error.to_string(),
    "database schema version 3 is unsupported; expected 2: remove the database and start a new run"
  );
  Ok(())
}

/// The run table as version 1 created it, with a run under way.
const VERSION_1_RUN: &str = "
create table run(
  id integer primary key check(id=1),
  daemon_seen_at int, stop_requested_at int, state_read_at int);
insert into run(id, state_read_at) values(1, 1700000000000);
pragma user_version=1;
";

#[test]
fn migrates_a_version_1_run_so_it_can_be_continued() -> Result<()> {
  let db = Connection::open_in_memory()?;
  db.execute_batch(VERSION_1_RUN)?;

  initialize_schema(&db)?;

  let version = db.query_row("pragma user_version", [], |row| row.get::<_, i64>(0))?;
  let run = db.query_row(
    "select state_read_at, observation_cursor, daemon_pid from run where id=1",
    [],
    |row| {
      Ok(format!(
        "read={} cursor={} daemon={:?}",
        row.get::<_, i64>(0)?,
        row.get::<_, i64>(1)?,
        row.get::<_, Option<i64>>(2)?,
      ))
    },
  )?;
  assert_eq!(version, 2);
  assert_eq!(run, "read=1700000000000 cursor=0 daemon=None");
  Ok(())
}

#[test]
fn initializes_one_database_concurrently() -> Result<()> {
  let suffix = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
  let path = std::env::temp_dir().join(format!(
    "chainsaw-schema-{}-{suffix}.db",
    std::process::id()
  ));
  let workers = 2;
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
