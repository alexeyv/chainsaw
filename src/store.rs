use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
begin immediate;
create table config(key text primary key, value text);
create table sessions(id integer primary key, name text not null, role text,
  pane_id text, tab_id text, external_session_id text unique, started_at real,
  context int default 0, context_max int default 0, last_growth real,
  kicked_at real, prepopulated_at real, stopped_at real, log_path text);
create table tasks(id integer primary key, text text, predicted_files int,
  predicted_lines int, state text, session_id int references sessions(id),
  commit_sha text, created_at real, retry_of_task_id int references tasks(id),
  reason text, log_offset int default 0, base_head text, predicted_file_list text,
  is_session_reuse int not null default 0, context_size_start int,
  context_size_end int);
create table task_events(task_id int, state text, at real);
create table prompts(id integer primary key, session text, text text,
  sent_at real, landed_at real, attempts int);
create table calibration(task_id int primary key, predicted_files int,
  predicted_lines int, actual_files int, actual_lines int, wall_seconds real,
  context_tokens int, recorded_at real, context_base int, context_end int);
create table dispositions(id integer primary key, finding text,
  verdict text, task_id int, reason text, at real);
create table human_waits(id integer primary key, started real, ended real);
create table events(at real, kind text, detail text);
pragma user_version=1;
commit;
"#;

pub const TASK_STATES: &[&str] = &[
  "drafted",
  "dispatched",
  "in_flight",
  "committed",
  "verified",
  "accepted",
  "ingested",
  "failed",
];

#[derive(Clone, Debug)]
pub struct Task {
  pub id: i64,
  pub text: String,
  pub predicted_files: i64,
  pub predicted_lines: i64,
  pub state: String,
  pub session_id: Option<i64>,
  pub commit_sha: Option<String>,
  pub retry_of_task_id: Option<i64>,
  pub reason: Option<String>,
  pub log_offset: i64,
  pub base_head: Option<String>,
  pub predicted_file_list: Option<String>,
  pub is_session_reuse: bool,
  pub context_size_start: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct Session {
  pub id: i64,
  pub name: String,
  pub role: String,
  pub external_session_id: Option<String>,
  pub log_path: Option<PathBuf>,
  pub context: i64,
  pub context_max: i64,
  pub last_growth: Option<f64>,
  pub kicked_at: Option<f64>,
  pub prepopulated_at: Option<f64>,
  pub stopped_at: Option<f64>,
}

pub struct Store {
  pub run_dir: PathBuf,
  pub logs_dir: PathBuf,
  pub path: PathBuf,
  pub db: Connection,
}

impl Store {
  pub fn open(run_dir: &Path) -> Result<Self> {
    let run_dir = run_dir
      .canonicalize()
      .with_context(|| format!("cannot resolve run directory {}", run_dir.display()))?;
    let home = env::var_os("HOME").context("HOME is not set")?;
    let munged = run_dir.to_string_lossy().replace(['/', '.'], "-");
    let logs_dir = PathBuf::from(home)
      .join(".claude")
      .join("projects")
      .join(munged);
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

  pub fn cfg_i64(&self, key: &str, default: i64) -> i64 {
    self
      .cfg(key)
      .ok()
      .flatten()
      .and_then(|value| value.parse().ok())
      .unwrap_or(default)
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

  pub fn set_task_state(&self, task_id: i64, state: &str) -> Result<()> {
    debug_assert!(TASK_STATES.contains(&state));
    let timestamp = now();
    self.db.execute(
      "update tasks set state=? where id=?",
      params![state, task_id],
    )?;
    self.db.execute(
      "insert into task_events values(?,?,?)",
      params![task_id, state, timestamp],
    )?;
    Ok(())
  }

  pub fn task(&self, id: i64) -> Result<Option<Task>> {
    self
      .db
      .query_row("select * from tasks where id=?", [id], task_from_row)
      .optional()
      .map_err(Into::into)
  }

  pub fn tasks(&self) -> Result<Vec<Task>> {
    let mut statement = self.db.prepare("select * from tasks order by id")?;
    let rows = statement.query_map([], task_from_row)?;
    rows
      .collect::<rusqlite::Result<Vec<_>>>()
      .map_err(Into::into)
  }

  pub fn session(&self, id: i64) -> Result<Option<Session>> {
    self
      .db
      .query_row("select * from sessions where id=?", [id], session_from_row)
      .optional()
      .map_err(Into::into)
  }

  pub fn latest_session_named(&self, name: &str) -> Result<Option<Session>> {
    self
      .db
      .query_row(
        "select * from sessions where name=? order by started_at desc, id desc limit 1",
        [name],
        session_from_row,
      )
      .optional()
      .map_err(Into::into)
  }

  pub fn sessions(&self) -> Result<Vec<Session>> {
    let mut statement = self
      .db
      .prepare("select * from sessions order by started_at")?;
    let rows = statement.query_map([], session_from_row)?;
    rows
      .collect::<rusqlite::Result<Vec<_>>>()
      .map_err(Into::into)
  }
}

pub fn now() -> f64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs_f64()
}

pub(crate) fn initialize_schema(db: &Connection) -> Result<()> {
  let version = db.query_row("pragma user_version", [], |row| row.get::<_, i64>(0))?;
  match version {
    0 => {
      let table_count = db.query_row(
        "select count(*) from sqlite_schema where type='table' and name not like 'sqlite_%'",
        [],
        |row| row.get::<_, i64>(0),
      )?;
      if table_count != 0 {
        bail!("database schema is unversioned; remove it before starting chainsaw");
      }
      db.execute_batch(SCHEMA)?;
    }
    SCHEMA_VERSION => {}
    version => bail!("database schema version {version} is unsupported; expected {SCHEMA_VERSION}"),
  }
  db.execute_batch("pragma foreign_keys=on;")?;
  Ok(())
}

fn task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
  Ok(Task {
    id: row.get("id")?,
    text: row.get("text")?,
    predicted_files: row.get("predicted_files")?,
    predicted_lines: row.get("predicted_lines")?,
    state: row.get("state")?,
    session_id: row.get("session_id")?,
    commit_sha: row.get("commit_sha")?,
    retry_of_task_id: row.get("retry_of_task_id")?,
    reason: row.get("reason")?,
    log_offset: row.get::<_, Option<i64>>("log_offset")?.unwrap_or_default(),
    base_head: row.get("base_head")?,
    predicted_file_list: row.get("predicted_file_list")?,
    is_session_reuse: row.get::<_, i64>("is_session_reuse")? != 0,
    context_size_start: row.get("context_size_start")?,
  })
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
  Ok(Session {
    id: row.get("id")?,
    name: row.get("name")?,
    role: row.get("role")?,
    external_session_id: row.get("external_session_id")?,
    log_path: row.get::<_, Option<String>>("log_path")?.map(PathBuf::from),
    context: row.get::<_, Option<i64>>("context")?.unwrap_or_default(),
    context_max: row
      .get::<_, Option<i64>>("context_max")?
      .unwrap_or_default(),
    last_growth: row.get("last_growth")?,
    kicked_at: row.get("kicked_at")?,
    prepopulated_at: row.get("prepopulated_at")?,
    stopped_at: row.get("stopped_at")?,
  })
}
