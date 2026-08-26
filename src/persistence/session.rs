use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::domain::{Role, Session};

const COLUMNS: &str = "
  id, name, role, external_session_id, launched_head, started_at, stopped_at,
  context, context_max, last_growth, kicked_at
";

/// Register a session that has just started. Its transcript has not grown yet,
/// so its last growth is its start.
pub fn create(
  transaction: &Transaction<'_>,
  name: &str,
  role: Role,
  external_session_id: &str,
  launched_head: Option<&str>,
) -> Result<Session> {
  let started_at = Utc::now();
  let id = transaction.query_row(
    "
      insert into sessions(
        name, role, external_session_id, launched_head, started_at, last_growth
      ) values (?1, ?2, ?3, ?4, ?5, ?5)
      returning id
      ",
    params![
      name,
      role.as_str(),
      external_session_id,
      launched_head,
      started_at.timestamp_millis(),
    ],
    |row| row.get(0),
  )?;
  get(transaction, id)?.with_context(|| format!("created session {id} is missing"))
}

pub fn get(transaction: &Transaction<'_>, id: i64) -> Result<Option<Session>> {
  transaction
    .query_row(
      &format!("select {COLUMNS} from sessions where id=?"),
      [id],
      session_row,
    )
    .optional()?
    .transpose()
}

/// The newest incarnation of `name`, live or not.
pub fn latest_named(transaction: &Transaction<'_>, name: &str) -> Result<Option<Session>> {
  transaction
    .query_row(
      &format!(
        "select {COLUMNS} from sessions where name=? order by started_at desc, id desc limit 1"
      ),
      [name],
      session_row,
    )
    .optional()?
    .transpose()
}

pub fn all(transaction: &Transaction<'_>) -> Result<Vec<Session>> {
  let mut statement = transaction.prepare(&format!(
    "select {COLUMNS} from sessions order by started_at, id"
  ))?;
  let rows = statement.query_map([], session_row)?;
  rows.map(|row| row?).collect()
}

/// Stop every live incarnation of `name`. Returns how many were stopped.
pub fn stop_named(transaction: &Transaction<'_>, name: &str) -> Result<usize> {
  let stopped = transaction.execute(
    "update sessions set stopped_at=? where name=? and stopped_at is null",
    params![Utc::now().timestamp_millis(), name],
  )?;
  Ok(stopped)
}

/// Record one poll's reading of the transcript. Growth moves the last-growth
/// mark to `at` and re-arms the kick; the maximum only ever rises.
pub fn record_reading(
  transaction: &Transaction<'_>,
  id: i64,
  context: i64,
  grew: bool,
  at: DateTime<Utc>,
) -> Result<Session> {
  transaction.execute(
    "
      update sessions set
        context=?1,
        context_max=max(context_max, ?1),
        last_growth=case when ?2 then ?3 else last_growth end,
        kicked_at=case when ?2 then null else kicked_at end
      where id=?4
      ",
    params![context, grew, at.timestamp_millis(), id],
  )?;
  get(transaction, id)?.with_context(|| format!("session {id} is missing"))
}

/// Latch that the session has been nudged; cleared by the next growth.
pub fn record_kick(transaction: &Transaction<'_>, id: i64) -> Result<Session> {
  transaction.execute(
    "update sessions set kicked_at=? where id=?",
    params![Utc::now().timestamp_millis(), id],
  )?;
  get(transaction, id)?.with_context(|| format!("session {id} is missing"))
}

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Session>> {
  let id: i64 = row.get("id")?;
  let name: String = row.get("name")?;
  let role: String = row.get("role")?;
  let external_session_id: String = row.get("external_session_id")?;
  let launched_head: Option<String> = row.get("launched_head")?;
  let started_at: i64 = row.get("started_at")?;
  let stopped_at: Option<i64> = row.get("stopped_at")?;
  let context: i64 = row.get("context")?;
  let context_max: i64 = row.get("context_max")?;
  let last_growth: i64 = row.get("last_growth")?;
  let kicked_at: Option<i64> = row.get("kicked_at")?;
  Ok((|| {
    Session::new(
      id,
      name,
      Role::try_from(role.as_str())?,
      external_session_id,
      launched_head,
      time(started_at, "started_at")?,
      stopped_at.map(|at| time(at, "stopped_at")).transpose()?,
      context,
      context_max,
      time(last_growth, "last_growth")?,
      kicked_at.map(|at| time(at, "kicked_at")).transpose()?,
    )
  })())
}

fn time(millis: i64, field: &str) -> Result<DateTime<Utc>> {
  DateTime::from_timestamp_millis(millis)
    .with_context(|| format!("session {field} is outside the supported range"))
}

#[cfg(test)]
mod tests;
