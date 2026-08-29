use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
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

pub fn delivered_at(transaction: &Transaction<'_>, task_id: i64) -> Result<Option<DateTime<Utc>>> {
  timestamp(transaction, "delivered_at", task_id)
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

pub fn woken_at(transaction: &Transaction<'_>, task_id: i64) -> Result<Option<DateTime<Utc>>> {
  timestamp(transaction, "woken_at", task_id)
}

fn timestamp(
  transaction: &Transaction<'_>,
  column: &str,
  task_id: i64,
) -> Result<Option<DateTime<Utc>>> {
  transaction
    .query_row(
      &format!("select {column} from commentary_deliveries where task_id=?"),
      [task_id],
      |row| row.get::<_, Option<i64>>(0),
    )
    .optional()?
    .flatten()
    .map(|millis| {
      DateTime::from_timestamp_millis(millis)
        .with_context(|| format!("commentary delivery {column} is outside the supported range"))
    })
    .transpose()
}

#[cfg(test)]
mod tests;
