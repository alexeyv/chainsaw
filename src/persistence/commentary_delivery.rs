use anyhow::Result;
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, params};

pub fn record(transaction: &Transaction<'_>, task_id: i64) -> Result<bool> {
  let delivered_at = Utc::now().timestamp_millis();
  let changed = transaction.execute(
    "insert into commentary_deliveries(task_id, delivered_at) values(?, ?)
     on conflict(task_id) do update set delivered_at=excluded.delivered_at
     where commentary_deliveries.delivered_at is null",
    params![task_id, delivered_at],
  )?;
  Ok(changed == 1)
}

pub fn delivered_at(transaction: &Transaction<'_>, task_id: i64) -> Result<Option<i64>> {
  transaction
    .query_row(
      "select delivered_at from commentary_deliveries where task_id=?",
      [task_id],
      |row| row.get::<_, Option<i64>>(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(Into::into)
}

pub fn record_wake(transaction: &Transaction<'_>, task_id: i64) -> Result<bool> {
  let woken_at = Utc::now().timestamp_millis();
  let changed = transaction.execute(
    "insert into commentary_deliveries(task_id, woken_at) values(?, ?)
     on conflict(task_id) do update set woken_at=excluded.woken_at
     where commentary_deliveries.woken_at is null",
    params![task_id, woken_at],
  )?;
  Ok(changed == 1)
}

pub fn woken_at(transaction: &Transaction<'_>, task_id: i64) -> Result<Option<i64>> {
  transaction
    .query_row(
      "select woken_at from commentary_deliveries where task_id=?",
      [task_id],
      |row| row.get::<_, Option<i64>>(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(Into::into)
}

#[cfg(test)]
mod tests;
