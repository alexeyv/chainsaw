use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::domain::Prompt;

struct PromptRow {
  id: i64,
  session_id: i64,
  text: String,
  sent_at: i64,
  seen_at: Option<i64>,
  attempts: i64,
}

/// Journals a prompt about to be sent to the session, not yet sent once.
pub fn create(transaction: &Transaction<'_>, session_id: i64, text: &str) -> Result<Prompt> {
  let sent_at = Utc::now().timestamp_millis();
  let id = transaction.query_row(
    "
      insert into prompts(session_id, text, sent_at, attempts)
      values (?1, ?2, ?3, 0) returning id
      ",
    params![session_id, text, sent_at],
    |row| row.get(0),
  )?;
  materialize(PromptRow {
    id,
    session_id,
    text: text.to_owned(),
    sent_at,
    seen_at: None,
    attempts: 0,
  })
}

pub fn get(transaction: &Transaction<'_>, id: i64) -> Result<Option<Prompt>> {
  transaction
    .query_row(
      "select id, session_id, text, sent_at, seen_at, attempts from prompts where id=?",
      [id],
      prompt_row,
    )
    .optional()?
    .map(materialize)
    .transpose()
}

/// Counts one more send of the prompt.
pub fn record_attempt(transaction: &Transaction<'_>, id: i64) -> Result<Prompt> {
  transaction.execute("update prompts set attempts=attempts+1 where id=?", [id])?;
  require(transaction, id)
}

/// Stamps the prompt as seen in its session's transcript just now.
pub fn record_seen(transaction: &Transaction<'_>, id: i64) -> Result<Prompt> {
  transaction.execute(
    "update prompts set seen_at=? where id=?",
    params![Utc::now().timestamp_millis(), id],
  )?;
  require(transaction, id)
}

fn require(transaction: &Transaction<'_>, id: i64) -> Result<Prompt> {
  get(transaction, id)?.with_context(|| format!("no prompt {id}"))
}

fn prompt_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PromptRow> {
  Ok(PromptRow {
    id: row.get("id")?,
    session_id: row.get("session_id")?,
    text: row.get("text")?,
    sent_at: row.get("sent_at")?,
    seen_at: row.get("seen_at")?,
    attempts: row.get("attempts")?,
  })
}

fn materialize(row: PromptRow) -> Result<Prompt> {
  let sent_at = time(row.sent_at, "sent_at")?;
  let seen_at = row.seen_at.map(|at| time(at, "seen_at")).transpose()?;
  Prompt::new(
    row.id,
    row.session_id,
    row.text,
    sent_at,
    seen_at,
    row.attempts,
  )
}

fn time(millis: i64, field: &str) -> Result<DateTime<Utc>> {
  DateTime::from_timestamp_millis(millis)
    .with_context(|| format!("prompt {field} is outside the supported range"))
}

#[cfg(test)]
mod tests;
