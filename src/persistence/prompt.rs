//! The prompts the supervisor has sent, and how each send fared: how many
//! times it went out and when the session's transcript first showed it.

use anyhow::Result;
use chrono::Utc;
use rusqlite::{Transaction, params};

/// Journals a prompt about to be sent to `session`; the id names it to the
/// records of its sends.
pub fn create(transaction: &Transaction<'_>, session: &str, text: &str) -> Result<i64> {
  let id = transaction.query_row(
    "
      insert into prompts(session, text, sent_at, attempts)
      values (?1, ?2, ?3, 0) returning id
      ",
    params![session, text, Utc::now().timestamp_millis()],
    |row| row.get(0),
  )?;
  Ok(id)
}

/// Counts one more send of the prompt.
pub fn record_attempt(transaction: &Transaction<'_>, id: i64) -> Result<()> {
  transaction.execute("update prompts set attempts=attempts+1 where id=?", [id])?;
  Ok(())
}

/// Stamps the prompt as seen in its session's transcript just now.
pub fn record_seen(transaction: &Transaction<'_>, id: i64) -> Result<()> {
  transaction.execute(
    "update prompts set seen_at=? where id=?",
    params![Utc::now().timestamp_millis(), id],
  )?;
  Ok(())
}

#[cfg(test)]
mod tests;
