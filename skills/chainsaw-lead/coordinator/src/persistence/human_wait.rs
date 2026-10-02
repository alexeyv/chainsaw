use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};

use crate::domain::HumanWait;

struct HumanWaitRow {
  id: i64,
  started: i64,
  ended: Option<i64>,
}

/// Opens a wait unless one is open already, and returns the open one.
pub fn start(transaction: &Transaction<'_>) -> Result<HumanWait> {
  if let Some(open) = open(transaction)? {
    return Ok(open);
  }
  let started = Utc::now().timestamp_millis();
  let id = transaction.query_row(
    "insert into human_waits(started) values(?) returning id",
    [started],
    |row| row.get(0),
  )?;
  materialize(HumanWaitRow {
    id,
    started,
    ended: None,
  })
}

/// Closes the open wait and returns it; None when no wait was open.
pub fn end(transaction: &Transaction<'_>) -> Result<Option<HumanWait>> {
  transaction
    .query_row(
      "update human_waits set ended=? where ended is null returning id, started, ended",
      [Utc::now().timestamp_millis()],
      human_wait_row,
    )
    .optional()?
    .map(materialize)
    .transpose()
}

/// The open wait, if any.
pub fn open(transaction: &Transaction<'_>) -> Result<Option<HumanWait>> {
  transaction
    .query_row(
      "select id, started, ended from human_waits where ended is null",
      [],
      human_wait_row,
    )
    .optional()?
    .map(materialize)
    .transpose()
}

/// Every wait, oldest first.
pub fn all(transaction: &Transaction<'_>) -> Result<Vec<HumanWait>> {
  let mut statement =
    transaction.prepare("select id, started, ended from human_waits order by id")?;
  let rows = statement.query_map([], human_wait_row)?;
  rows.map(|row| materialize(row?)).collect()
}

fn human_wait_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HumanWaitRow> {
  Ok(HumanWaitRow {
    id: row.get("id")?,
    started: row.get("started")?,
    ended: row.get("ended")?,
  })
}

fn materialize(row: HumanWaitRow) -> Result<HumanWait> {
  let started = time(row.started, "started")?;
  let ended = row.ended.map(|at| time(at, "ended")).transpose()?;
  HumanWait::new(row.id, started, ended)
}

fn time(millis: i64, field: &str) -> Result<DateTime<Utc>> {
  DateTime::from_timestamp_millis(millis)
    .with_context(|| format!("human wait {field} is outside the supported range"))
}

#[cfg(test)]
mod tests;
