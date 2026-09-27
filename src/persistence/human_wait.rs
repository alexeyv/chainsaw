//! The intervals the run spent waiting on the human. At most one is open at
//! a time; an open one has no end.

use anyhow::Result;
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction};

/// Opens a wait unless one is open already. True when this call opened it.
pub fn start(transaction: &Transaction<'_>) -> Result<bool> {
  if is_open(transaction)? {
    return Ok(false);
  }
  transaction.execute(
    "insert into human_waits(started) values(?)",
    [Utc::now().timestamp_millis()],
  )?;
  Ok(true)
}

/// Closes the open wait, if any. True when there was one.
pub fn end(transaction: &Transaction<'_>) -> Result<bool> {
  let closed = transaction.execute(
    "update human_waits set ended=? where ended is null",
    [Utc::now().timestamp_millis()],
  )?;
  Ok(closed > 0)
}

pub fn is_open(transaction: &Transaction<'_>) -> Result<bool> {
  let open: Option<i64> = transaction
    .query_row("select 1 from human_waits where ended is null", [], |row| {
      row.get(0)
    })
    .optional()?;
  Ok(open.is_some())
}

/// Every wait as (started, ended) in milliseconds since the epoch, oldest
/// first; an open wait has no end.
pub fn intervals(transaction: &Transaction<'_>) -> Result<Vec<(i64, Option<i64>)>> {
  let mut statement = transaction.prepare("select started, ended from human_waits order by id")?;
  let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
  Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests;
