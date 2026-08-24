use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = r#"
create table if not exists config(key text primary key, value text);
create table if not exists tasks(id integer primary key, text text, predicted_files int,
  predicted_lines int, state text, implementer text, commit_sha text, created_at real,
  retry_of int, reason text, log_offset int default 0, base_head text,
  predicted_file_list text, reuse int default 0, context_base int);
create table if not exists task_log(task_id int, state text, at real);
create table if not exists sessions(name text primary key, role text, pane_id text,
  tab_id text, session_id text, started_at real, context int default 0,
  context_max int default 0, last_growth real, kicked_at real,
  prepopulated_at real, stopped_at real);
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

const MIGRATIONS: &[&str] = &[
    "alter table tasks add column predicted_file_list text",
    "alter table tasks add column reuse int default 0",
    "alter table tasks add column context_base int",
    "alter table calibration add column context_base int",
    "alter table calibration add column context_end int",
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
    pub implementer: Option<String>,
    pub commit_sha: Option<String>,
    pub retry_of: Option<i64>,
    pub reason: Option<String>,
    pub log_offset: i64,
    pub base_head: Option<String>,
    pub predicted_file_list: Option<String>,
    pub reuse: bool,
    pub context_base: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub name: String,
    pub role: String,
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
        for migration in MIGRATIONS {
            let _ = db.execute(migration, []);
        }
        Ok(Self {
            run_dir,
            logs_dir,
            path,
            db,
        })
    }

    pub fn cfg(&self, key: &str) -> Result<Option<String>> {
        self.db
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
        self.cfg(key)
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
            "insert into task_log values(?,?,?)",
            params![task_id, state, timestamp],
        )?;
        Ok(())
    }

    pub fn task(&self, id: i64) -> Result<Option<Task>> {
        self.db
            .query_row("select * from tasks where id=?", [id], task_from_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn tasks(&self) -> Result<Vec<Task>> {
        let mut statement = self.db.prepare("select * from tasks order by id")?;
        let rows = statement.query_map([], task_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn session(&self, name: &str) -> Result<Option<Session>> {
        self.db
            .query_row(
                "select * from sessions where name=?",
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
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get("id")?,
        text: row.get("text")?,
        predicted_files: row.get("predicted_files")?,
        predicted_lines: row.get("predicted_lines")?,
        state: row.get("state")?,
        implementer: row.get("implementer")?,
        commit_sha: row.get("commit_sha")?,
        retry_of: row.get("retry_of")?,
        reason: row.get("reason")?,
        log_offset: row.get::<_, Option<i64>>("log_offset")?.unwrap_or_default(),
        base_head: row.get("base_head")?,
        predicted_file_list: row.get("predicted_file_list")?,
        reuse: row.get::<_, Option<i64>>("reuse")?.unwrap_or_default() != 0,
        context_base: row.get("context_base")?,
    })
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        name: row.get("name")?,
        role: row.get("role")?,
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
