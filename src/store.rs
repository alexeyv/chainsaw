use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
create table config(key text primary key, value text);
create table sessions(
  id integer primary key, name text not null, role text not null,
  external_session_id text not null unique, launched_head text,
  started_at int not null, stopped_at int,
  context int not null default 0, context_max int not null default 0,
  last_growth int not null, kicked_at int);
create table tasks(id integer primary key, text text, predicted_files int,
  predicted_lines int, session_id int references sessions(id),
  commit_sha text, created_at real, retry_of_task_id int references tasks(id),
  log_offset int default 0, base_head text, predicted_file_list text,
  is_session_reuse int not null default 0, context_size_start int);
create table task_events(
  id integer primary key autoincrement,
  task_id int not null references tasks(id), state text not null,
  reason text, created_at int not null);
create table commentary_deliveries(
  task_id int primary key references tasks(id), delivered_at int not null);
create table prompts(id integer primary key, session text, text text,
  sent_at real, landed_at real, attempts int);
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
create table human_waits(id integer primary key, started real, ended real);
create table events(at real, kind text, detail text);
pragma user_version=1;
"#;

pub struct Store {
  pub run_dir: PathBuf,
  pub logs_dir: PathBuf,
  pub path: PathBuf,
  pub db: Connection,
}

/// Claude Code names a project directory after the session's cwd, replacing
/// separators with dashes and nothing else. Dots survive.
fn project_directory_name(canonical_run_dir: &Path) -> String {
  canonical_run_dir.to_string_lossy().replace('/', "-")
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

  pub fn cfg(&self, key: &str) -> Result<Option<String>> {
    self
      .db
      .query_row("select value from config where key=?", [key], |row| {
        row.get(0)
      })
      .optional()
      .map_err(Into::into)
  }

  pub fn cfg_or(&self, key: &str, default: &str) -> Result<String> {
    Ok(self.cfg(key)?.unwrap_or_else(|| default.to_owned()))
  }

  pub fn set_cfg(&self, key: &str, value: &str) -> Result<()> {
    self.db.execute(
      "insert or replace into config values(?,?)",
      params![key, value],
    )?;
    Ok(())
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

pub fn now() -> f64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs_f64()
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
mod tests {
  use std::fs;
  use std::path::PathBuf;
  use std::sync::atomic::{AtomicU64, Ordering};
  use std::sync::{Arc, Barrier};
  use std::thread;
  use std::time::Duration;

  use anyhow::Result;
  use rusqlite::{Connection, Transaction, TransactionBehavior};

  use super::{Store, initialize_schema, project_directory_name};

  static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);

  mod project_directory_name {
    use std::path::Path;

    use super::project_directory_name;

    #[test]
    fn should_work() {
      assert_eq!(
        project_directory_name(Path::new("/Users/a/src/chainsaw")),
        "-Users-a-src-chainsaw"
      );
    }

    #[test]
    fn should_keep_dots_when_the_run_directory_is_dotted() {
      assert_eq!(
        project_directory_name(Path::new("/Users/a/src/ui.wt/refactor")),
        "-Users-a-src-ui.wt-refactor"
      );
    }
  }

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
    let commentary_delivery_columns = db.query_row(
      "select count(*) from pragma_table_info('commentary_deliveries')",
      [],
      |row| row.get::<_, i64>(0),
    )?;
    assert_eq!(version, 1);
    assert_eq!(task_id_required, 1);
    assert_eq!(task_foreign_keys, 2);
    assert_eq!(observation_foreign_keys, 1);
    assert_eq!(task_state_columns, 0);
    assert_eq!(legacy_finding_columns, 0);
    assert_eq!(commentary_delivery_columns, 2);
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
}
