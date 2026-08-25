use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = r#"
create table if not exists config(key text primary key, value text);
create table if not exists sessions(id integer primary key, name text not null, role text,
  pane_id text, tab_id text, external_session_id text unique, started_at real,
  context int default 0, context_max int default 0, last_growth real,
  kicked_at real, prepopulated_at real, stopped_at real, log_path text);
create table if not exists tasks(id integer primary key, text text, predicted_files int,
  predicted_lines int, state text, session_id int references sessions(id),
  commit_sha text, created_at real, retry_of_task_id int references tasks(id),
  reason text, log_offset int default 0, base_head text, predicted_file_list text,
  is_session_reuse int not null default 0, context_size_start int,
  context_size_end int);
create table if not exists taskEvents(task_id int, state text, at real);
create table if not exists prompts(id integer primary key, session text, text text,
  sent_at real, landed_at real, attempts int);
create table if not exists calibration(task_id int primary key, predicted_files int,
  predicted_lines int, actual_files int, actual_lines int, wall_seconds real,
  context_tokens int, recorded_at real, context_base int, context_end int);
create table if not exists dispositions(id integer primary key, finding text,
  verdict text, task_id int, reason text, at real);
create table if not exists human_waits(id integer primary key, started real, ended real);
create table if not exists events(at real, kind text, detail text);
"#;

const ADD_COLUMN_MIGRATIONS: &[(&str, &str, &str)] = &[
  (
    "tasks",
    "predicted_file_list",
    "alter table tasks add column predicted_file_list text",
  ),
  (
    "tasks",
    "is_session_reuse",
    "alter table tasks add column is_session_reuse int not null default 0",
  ),
  (
    "tasks",
    "context_size_start",
    "alter table tasks add column context_size_start int",
  ),
  (
    "tasks",
    "context_size_end",
    "alter table tasks add column context_size_end int",
  ),
  (
    "calibration",
    "context_base",
    "alter table calibration add column context_base int",
  ),
  (
    "calibration",
    "context_end",
    "alter table calibration add column context_end int",
  ),
  (
    "sessions",
    "log_path",
    "alter table sessions add column log_path text",
  ),
];

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
    db.execute_batch(SCHEMA)?;
    apply_migrations(&db)?;
    db.execute_batch("pragma foreign_keys=on;")?;
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
      "insert into taskEvents values(?,?,?)",
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

fn apply_migrations(db: &Connection) -> Result<()> {
  migrate_task_events_table(db)?;
  if column_exists(db, "tasks", "retry_of")? && !column_exists(db, "tasks", "retry_of_task_id")? {
    db.execute(
      "alter table tasks rename column retry_of to retry_of_task_id",
      [],
    )?;
  }
  if column_exists(db, "tasks", "reuse")? && !column_exists(db, "tasks", "is_session_reuse")? {
    db.execute(
      "alter table tasks rename column reuse to is_session_reuse",
      [],
    )?;
  }
  if column_exists(db, "tasks", "context_base")?
    && !column_exists(db, "tasks", "context_size_start")?
  {
    db.execute(
      "alter table tasks rename column context_base to context_size_start",
      [],
    )?;
  }
  for (table, column, migration) in ADD_COLUMN_MIGRATIONS {
    if !column_exists(db, table, column)? {
      db.execute(migration, [])?;
    }
  }
  if !column_exists(db, "sessions", "id")? {
    migrate_session_identity(db)?;
  }
  Ok(())
}

fn migrate_task_events_table(db: &Connection) -> Result<()> {
  if !table_exists(db, "task_log")? {
    return Ok(());
  }
  if table_exists(db, "taskEvents")? {
    db.execute_batch(
      "
          begin immediate;
          insert into taskEvents(task_id, state, at)
            select task_id, state, at from task_log;
          drop table task_log;
          commit;
          ",
    )?;
  } else {
    db.execute("alter table task_log rename to taskEvents", [])?;
  }
  Ok(())
}

fn migrate_session_identity(db: &Connection) -> Result<()> {
  db.execute_batch(
    "
        begin immediate;
        alter table sessions rename to sessions_legacy;
        create table sessions(
          id integer primary key,
          name text not null,
          role text,
          pane_id text,
          tab_id text,
          external_session_id text unique,
          started_at real,
          context int default 0,
          context_max int default 0,
          last_growth real,
          kicked_at real,
          prepopulated_at real,
          stopped_at real,
          log_path text
        );
        insert into sessions(
          name, role, pane_id, tab_id, external_session_id, started_at, context,
          context_max, last_growth, kicked_at, prepopulated_at, stopped_at, log_path
        )
        select name, role, pane_id, tab_id, session_id, started_at, context,
          context_max, last_growth, kicked_at, prepopulated_at, stopped_at, log_path
        from sessions_legacy;

        alter table tasks rename to tasks_legacy;
        create table tasks(
          id integer primary key,
          text text,
          predicted_files int,
          predicted_lines int,
          state text,
          session_id int references sessions(id),
          commit_sha text,
          created_at real,
          retry_of_task_id int references tasks(id),
          reason text,
          log_offset int default 0,
          base_head text,
          predicted_file_list text,
          is_session_reuse int not null default 0,
          context_size_start int,
          context_size_end int
        );
        insert into tasks(
          id, text, predicted_files, predicted_lines, state, session_id, commit_sha,
          created_at, retry_of_task_id, reason, log_offset, base_head,
          predicted_file_list, is_session_reuse, context_size_start,
          context_size_end
        )
        select t.id, t.text, t.predicted_files, t.predicted_lines, t.state,
          (select s.id from sessions s where s.name=t.implementer
           order by s.started_at desc, s.id desc limit 1),
          t.commit_sha, t.created_at, t.retry_of_task_id, t.reason, t.log_offset,
          t.base_head, t.predicted_file_list, t.is_session_reuse,
          t.context_size_start, t.context_size_end
        from tasks_legacy t;
        drop table tasks_legacy;
        drop table sessions_legacy;
        commit;
        ",
  )?;
  Ok(())
}

fn column_exists(db: &Connection, table: &str, column: &str) -> Result<bool> {
  let mut statement = db.prepare(&format!("pragma table_info({table})"))?;
  let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
  for existing in columns {
    if existing? == column {
      return Ok(true);
    }
  }
  Ok(false)
}

fn table_exists(db: &Connection, table: &str) -> Result<bool> {
  db.query_row(
    "select 1 from sqlite_master where type='table' and name=?",
    [table],
    |_| Ok(()),
  )
  .optional()
  .map(|result| result.is_some())
  .map_err(Into::into)
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

#[cfg(test)]
mod tests {
  use super::{SCHEMA, apply_migrations, column_exists, table_exists};
  use rusqlite::Connection;

  #[test]
  fn migrates_legacy_task_relationships_to_stable_ids() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
      "
            create table sessions(
              name text primary key, role text, pane_id text, tab_id text,
              session_id text, started_at real, context int default 0,
              context_max int default 0, last_growth real, kicked_at real,
              prepopulated_at real, stopped_at real
            );
            create table tasks(
              id integer primary key, text text, predicted_files int,
              predicted_lines int, state text, implementer text, commit_sha text,
              created_at real, retry_of int, reason text, log_offset int default 0,
              base_head text, reuse int default 0, context_base int
            );
            create table calibration(task_id int primary key);
            insert into sessions(name, role, session_id, started_at)
              values('worker', 'implementer', 'external-123', 10.0);
            insert into tasks(
              id, text, predicted_files, predicted_lines, state, implementer,
              created_at, retry_of
            ) values(1, 'first', 1, 10, 'failed', 'worker', 11.0, null);
            insert into tasks(
              id, text, predicted_files, predicted_lines, state, implementer,
              created_at, retry_of, reuse, context_base
            ) values(2, 'retry', 1, 10, 'drafted', 'worker', 12.0, 1, 1, 42);
            ",
    )
    .unwrap();

    apply_migrations(&db).unwrap();

    assert!(column_exists(&db, "tasks", "retry_of_task_id").unwrap());
    assert!(!column_exists(&db, "tasks", "retry_of").unwrap());
    assert!(column_exists(&db, "tasks", "session_id").unwrap());
    assert!(!column_exists(&db, "tasks", "implementer").unwrap());
    assert!(column_exists(&db, "sessions", "id").unwrap());
    assert!(column_exists(&db, "sessions", "external_session_id").unwrap());
    let (retry_of_task_id, session_id, is_session_reuse, context_size_start): (i64, i64, i64, i64) =
      db.query_row(
        "select retry_of_task_id, session_id, is_session_reuse, context_size_start
                 from tasks where id=2",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
      )
      .unwrap();
    assert_eq!(retry_of_task_id, 1);
    assert_eq!(is_session_reuse, 1);
    assert_eq!(context_size_start, 42);
    let context_size_end: Option<i64> = db
      .query_row("select context_size_end from tasks where id=2", [], |row| {
        row.get(0)
      })
      .unwrap();
    assert_eq!(context_size_end, None);
    let external_session_id: String = db
      .query_row(
        "select external_session_id from sessions where id=?",
        [session_id],
        |row| row.get(0),
      )
      .unwrap();
    assert_eq!(external_session_id, "external-123");
  }

  #[test]
  fn renames_task_log_table_without_losing_rows() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
      "
          create table task_log(task_id int, state text, at real);
          insert into task_log values(7, 'drafted', 1.0);
          insert into task_log values(7, 'in_flight', 2.0);
          ",
    )
    .unwrap();
    db.execute_batch(SCHEMA).unwrap();

    apply_migrations(&db).unwrap();

    assert!(!table_exists(&db, "task_log").unwrap());
    assert!(table_exists(&db, "taskEvents").unwrap());
    let rows = db
      .prepare("select task_id, state, at from taskEvents order by at")
      .unwrap()
      .query_map([], |row| {
        Ok((
          row.get::<_, i64>(0)?,
          row.get::<_, String>(1)?,
          row.get::<_, f64>(2)?,
        ))
      })
      .unwrap()
      .collect::<rusqlite::Result<Vec<_>>>()
      .unwrap();
    assert_eq!(
      rows,
      vec![
        (7, "drafted".to_owned(), 1.0),
        (7, "in_flight".to_owned(), 2.0)
      ]
    );
  }
}
