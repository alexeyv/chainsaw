use anyhow::Result;
use chrono::Utc;
use rusqlite::{Transaction, params};

use crate::domain::{TaskEvent, TaskState};

pub(super) fn create(
  transaction: &Transaction<'_>,
  task_id: i64,
  state: TaskState,
  reason: Option<&str>,
) -> Result<TaskEvent> {
  let created_at = Utc::now();
  let id = transaction.query_row(
    "
      insert into task_events(task_id, state, reason, created_at)
      values (?1, ?2, ?3, ?4)
      returning id
      ",
    params![
      task_id,
      state.as_str(),
      reason,
      created_at.timestamp_millis()
    ],
    |row| row.get(0),
  )?;
  let event = TaskEvent::new(id, state, reason.map(str::to_owned), created_at)?;
  Ok(event)
}

#[cfg(test)]
mod tests;
