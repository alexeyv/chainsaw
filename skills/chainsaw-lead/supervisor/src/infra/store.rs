use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
create table run(
  id integer primary key check(id=1),
  daemon_seen_at int, stop_requested_at int, state_read_at int);
insert into run(id) values(1);
create table sessions(
  id integer primary key, name text not null, role text not null,
  external_session_id text not null unique, launched_head text,
  started_at int not null, stopped_at int,
  context int not null default 0, context_max int not null default 0,
  last_growth int not null, kicked_at int, over_limit_at int);
create table tasks(id integer primary key, text text, predicted_files int,
  predicted_lines int, session_id int references sessions(id),
  commit_sha text, created_at int, retry_of_task_id int references tasks(id),
  log_offset int default 0, base_head text, predicted_file_list text,
  context_size_start int, commentary_requested_at int, commentary_delivered_at int);
create table task_events(
  id integer primary key autoincrement,
  task_id int not null references tasks(id), state text not null,
  reason text, created_at int not null);
create table prompts(id integer primary key, session text, text text,
  sent_at int, landed_at int, attempts int);
create table calibrations(
  id integer primary key autoincrement,
  task_id int not null unique references tasks(id),
  predicted_files int not null, predicted_lines int not null,
  actual_files int not null, actual_lines int not null, wall_seconds real,
  created_at int not null, context_size_start int not null,
  context_size_end int not null);
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
create table events(at int, kind text, detail text);
pragma user_version=1;
"#;

pub struct Store {
  pub run_dir: PathBuf,
  pub logs_dir: PathBuf,
  pub path: PathBuf,
  pub db: Connection,
}

/// Claude Code names a project directory after the session's cwd, replacing
/// both separators and dots with dashes: `/Users/alex/src/ui.wt/run` becomes
/// `-Users-alex-src-ui-wt-run`, and `/x/.bare` becomes `-x--bare`. Keeping the
/// dots put the database beside no transcript at all, and the commentator's
/// start message named a directory holding nothing (run of 2026-08-28).
fn project_directory_name(canonical_run_dir: &Path) -> String {
  canonical_run_dir.to_string_lossy().replace(['/', '.'], "-")
}

/// Where Claude Code keeps a session's transcripts. The supervisor's own database
/// lives here too, so a run's state sits beside the logs it is derived from.
pub fn logs_dir_for(canonical_run_dir: &Path) -> Result<PathBuf> {
  let home = env::var_os("HOME").context("HOME is not set")?;
  Ok(
    PathBuf::from(home)
      .join(".claude")
      .join("projects")
      .join(project_directory_name(canonical_run_dir)),
  )
}

impl Store {
  pub fn open(run_dir: &Path) -> Result<Self> {
    let run_dir = run_dir
      .canonicalize()
      .with_context(|| format!("cannot resolve run directory {}", run_dir.display()))?;
    let logs_dir = logs_dir_for(&run_dir)?;
    fs::create_dir_all(&logs_dir)?;
    let path = logs_dir.join("chainsaw-supervisor.db");
    let db = Connection::open(&path)?;
    db.busy_timeout(Duration::from_secs(30))?;
    initialize_schema(&db)?;
    Ok(Self {
      run_dir,
      logs_dir,
      path,
      db,
    })
  }

  pub fn event(&self, kind: &str, detail: &str) -> Result<()> {
    self.db.execute(
      "insert into events values(?,?,?)",
      params![now(), kind, detail],
    )?;
    Ok(())
  }

  /// Reserve the SQLite writer lock before any reads can make an upgrade fail fast.
  pub fn write_transaction(&self) -> Result<Transaction<'_>> {
    Ok(Transaction::new_unchecked(
      &self.db,
      TransactionBehavior::Immediate,
    )?)
  }
}

/// Milliseconds since the epoch: the unit of every stored timestamp.
pub fn now() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as i64
}

pub(crate) fn initialize_schema(db: &Connection) -> Result<()> {
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
    SCHEMA_VERSION => {}
    version => bail!(
      "database schema version {version} is unsupported; expected {SCHEMA_VERSION}: remove the database and start a new run"
    ),
  }
  transaction.commit()?;
  db.execute_batch("pragma foreign_keys=on;")?;
  Ok(())
}

#[cfg(test)]
mod tests;
