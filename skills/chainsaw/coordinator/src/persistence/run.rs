use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};

use crate::domain::Run;

struct RunRow {
  daemon_seen_at: Option<i64>,
  stop_requested_at: Option<i64>,
  state_read_at: Option<i64>,
  observation_cursor: i64,
  daemon_pid: Option<i64>,
}

/// The run record is a singleton seeded by the schema, so a missing row is a
/// damaged database, never a run that has not started.
pub fn get(transaction: &Transaction<'_>) -> Result<Run> {
  let row = transaction
    .query_row(
      "select daemon_seen_at, stop_requested_at, state_read_at, observation_cursor, daemon_pid
       from run where id=1",
      [],
      run_row,
    )
    .optional()?
    .context("run record is missing")?;
  materialize(row)
}

/// Stamp that a daemon polled just now.
pub fn record_daemon_seen(transaction: &Transaction<'_>) -> Result<Run> {
  stamp(transaction, "daemon_seen_at", Some(Utc::now()))
}

/// Ask the run to stop. A repeated request moves the time to the latest one.
pub fn request_stop(transaction: &Transaction<'_>) -> Result<Run> {
  stamp(transaction, "stop_requested_at", Some(Utc::now()))
}

/// Withdraw any standing stop request, as a starting daemon does.
pub fn clear_stop_request(transaction: &Transaction<'_>) -> Result<Run> {
  stamp(transaction, "stop_requested_at", None)
}

/// Record that the daemon with process id `pid` now owns the run, which
/// supersedes any daemon that started before it.
pub fn claim_daemon(transaction: &Transaction<'_>, pid: u32) -> Result<Run> {
  update(transaction, "daemon_pid=?", [i64::from(pid)])
}

/// Record that the lead has been delivered the observations up to `cursor`.
/// The cursor never moves back, so a lead re-reading from an earlier cursor
/// does not make a continuation read the same observations again.
pub fn record_observation_cursor(transaction: &Transaction<'_>, cursor: i64) -> Result<Run> {
  update(
    transaction,
    "observation_cursor=max(observation_cursor, ?)",
    [cursor],
  )
}

/// Stamp that state was read just now.
pub fn record_state_read(transaction: &Transaction<'_>) -> Result<Run> {
  stamp(transaction, "state_read_at", Some(Utc::now()))
}

fn stamp(transaction: &Transaction<'_>, column: &str, at: Option<DateTime<Utc>>) -> Result<Run> {
  update(
    transaction,
    &format!("{column}=?"),
    [at.map(|at| at.timestamp_millis())],
  )
}

fn update(
  transaction: &Transaction<'_>,
  assignment: &str,
  params: impl rusqlite::Params,
) -> Result<Run> {
  let changed = transaction.execute(&format!("update run set {assignment} where id=1"), params)?;
  if changed == 0 {
    anyhow::bail!("run record is missing");
  }
  get(transaction)
}

fn run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
  Ok(RunRow {
    daemon_seen_at: row.get("daemon_seen_at")?,
    stop_requested_at: row.get("stop_requested_at")?,
    state_read_at: row.get("state_read_at")?,
    observation_cursor: row.get("observation_cursor")?,
    daemon_pid: row.get("daemon_pid")?,
  })
}

fn materialize(row: RunRow) -> Result<Run> {
  Run::new(
    row
      .daemon_seen_at
      .map(|at| time(at, "daemon_seen_at"))
      .transpose()?,
    row
      .stop_requested_at
      .map(|at| time(at, "stop_requested_at"))
      .transpose()?,
    row
      .state_read_at
      .map(|at| time(at, "state_read_at"))
      .transpose()?,
    row.observation_cursor,
    row
      .daemon_pid
      .map(|pid| u32::try_from(pid).context("run daemon_pid is not a process id"))
      .transpose()?,
  )
}

fn time(millis: i64, field: &str) -> Result<DateTime<Utc>> {
  DateTime::from_timestamp_millis(millis)
    .with_context(|| format!("run {field} is outside the supported range"))
}

#[cfg(test)]
mod tests;
