use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use rusqlite::{Connection, ErrorCode, Transaction, TransactionBehavior};

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r#"
create table run(
  id integer primary key check(id=1),
  daemon_seen_at int, stop_requested_at int, state_read_at int,
  observation_cursor int not null default 0, daemon_pid int);
insert into run(id) values(1);
create table sessions(
  id integer primary key, name text not null, role text not null,
  agent text not null, external_session_id text not null unique,
  launched_head text,
  started_at int not null, stopped_at int,
  context int, context_max int,
  last_growth int not null, kicked_at int, over_limit_at int,
  transcript text not null, pane_id text, role_prompt text);
create table tasks(id integer primary key, text text, predicted_files int,
  predicted_lines int, session_id int references sessions(id),
  commit_sha text, created_at int, retry_of_task_id int references tasks(id),
  transcript_offset int default 0, base_head text, predicted_file_list text,
  context_size_start int, commentary_requested_at int, commentary_delivered_at int);
create table task_events(
  id integer primary key autoincrement,
  task_id int not null references tasks(id), state text not null,
  reason text, created_at int not null);
create table prompts(
  id integer primary key,
  session_id int not null references sessions(id), text text not null,
  sent_at int not null, seen_at int, attempts int not null);
create table calibrations(
  id integer primary key autoincrement,
  task_id int not null unique references tasks(id),
  predicted_files int not null, predicted_lines int not null,
  actual_files int not null, actual_lines int not null, wall_seconds real,
  created_at int not null, context_size_start int,
  context_size_end int);
create table observations(
  id integer primary key autoincrement,
  task_id int references tasks(id), text text not null,
  created_at int not null);
create table findings(
  id integer primary key autoincrement,
  task_id int not null references tasks(id),
  description text not null, verdict text,
  verdict_reason text, fix_task_id int references tasks(id),
  created_at int not null, resolved_at int);
create table human_waits(id integer primary key, started int, ended int);
create table run_events(
  id integer primary key autoincrement,
  kind text not null, detail text not null, created_at int not null);
pragma user_version=2;
"#;

/// Brings a version 1 database, from before runs could be continued, to
/// version 2: the run record learns the lead's observation cursor and which
/// daemon owns the run.
const MIGRATE_FROM_1: &str = r#"
alter table run add column observation_cursor int not null default 0;
alter table run add column daemon_pid int;
pragma user_version=2;
"#;

pub const DATABASE_FILE_NAME: &str = "chainsaw-supervisor.db";
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Store {
  pub path: PathBuf,
  pub db: Connection,
}

impl Store {
  /// Opens the database in the run's state directory, which must exist.
  pub fn open(state_dir: &Path) -> Result<Self> {
    let path = state_dir.join(DATABASE_FILE_NAME);
    let db = Connection::open(&path)?;
    db.busy_timeout(BUSY_TIMEOUT)?;
    initialize_schema(&db)?;
    Ok(Self { path, db })
  }

  /// Reserve the SQLite writer lock before any reads can make an upgrade fail fast.
  pub fn write_transaction(&self) -> Result<Transaction<'_>> {
    Ok(Transaction::new_unchecked(
      &self.db,
      TransactionBehavior::Immediate,
    )?)
  }

  /// Runs `work` in one read transaction: a consistent snapshot of the store.
  pub fn read<T>(&self, work: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
    let transaction = self.db.unchecked_transaction()?;
    let value = work(&transaction)?;
    transaction.commit()?;
    Ok(value)
  }

  /// Runs `work` in one write transaction, committed when it returns Ok and
  /// rolled back when it returns Err.
  pub fn write<T>(&self, work: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
    let transaction = self.write_transaction()?;
    let value = work(&transaction)?;
    transaction.commit()?;
    Ok(value)
  }
}

pub(crate) fn initialize_schema(db: &Connection) -> Result<()> {
  enable_wal(db)?;
  let version = db.query_row("pragma user_version", [], |row| row.get::<_, i64>(0))?;
  if version != SCHEMA_VERSION {
    let transaction = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    let version = transaction.query_row("pragma user_version", [], |row| row.get::<_, i64>(0))?;
    match version {
      0 => {
        let table_count = transaction.query_row(
          "select count(*) from sqlite_schema where type='table' and name not like 'sqlite_%'",
          [],
          |row| row.get::<_, i64>(0),
        )?;
        if table_count != 0 {
          bail!("database schema is unversioned; remove it before starting chainsaw");
        }
        transaction.execute_batch(SCHEMA)?;
      }
      1 => transaction.execute_batch(MIGRATE_FROM_1)?,
      SCHEMA_VERSION => {}
      version => bail!(
        "database schema version {version} is unsupported; expected {SCHEMA_VERSION}: remove the database and start a new run"
      ),
    }
    transaction.commit()?;
  }
  db.execute_batch("pragma foreign_keys=on;")?;
  Ok(())
}

/// Switches a file-backed database to WAL mode once. SQLite can return
/// `SQLITE_BUSY` immediately when two connections race to switch journal mode
/// on a brand-new database, so retry briefly until the mode sticks.
fn enable_wal(db: &Connection) -> Result<()> {
  let deadline = Instant::now() + BUSY_TIMEOUT;
  loop {
    let mode: String = match db.query_row("pragma journal_mode", [], |row| row.get(0)) {
      Ok(mode) => mode,
      Err(error) if is_busy(&error) && Instant::now() < deadline => {
        thread::sleep(Duration::from_millis(10));
        continue;
      }
      Err(error) => return Err(error.into()),
    };
    if mode.eq_ignore_ascii_case("wal") || mode.eq_ignore_ascii_case("memory") {
      return Ok(());
    }
    match db.execute_batch("pragma journal_mode=wal;") {
      Ok(()) => return Ok(()),
      Err(error) if is_busy(&error) && Instant::now() < deadline => {
        thread::sleep(Duration::from_millis(10));
      }
      Err(error) => return Err(error.into()),
    }
  }
}

fn is_busy(error: &rusqlite::Error) -> bool {
  matches!(
    error.sqlite_error_code(),
    Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
  )
}

#[cfg(test)]
mod tests;
