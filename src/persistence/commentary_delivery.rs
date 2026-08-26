use anyhow::Result;
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, params};

pub fn record(transaction: &Transaction<'_>, task_id: i64) -> Result<bool> {
  let delivered_at = Utc::now().timestamp_millis();
  let changed = transaction.execute(
    "insert or ignore into commentary_deliveries(task_id, delivered_at) values(?, ?)",
    params![task_id, delivered_at],
  )?;
  Ok(changed == 1)
}

pub fn delivered_at(transaction: &Transaction<'_>, task_id: i64) -> Result<Option<i64>> {
  transaction
    .query_row(
      "select delivered_at from commentary_deliveries where task_id=?",
      [task_id],
      |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

#[cfg(test)]
mod tests;
