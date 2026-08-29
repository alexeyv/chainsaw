use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Transaction, params};

use crate::domain::Observation;

struct ObservationRow {
  id: i64,
  task_id: Option<i64>,
  text: String,
  created_at: i64,
}

pub fn create(
  transaction: &Transaction<'_>,
  task_id: Option<i64>,
  text: &str,
) -> Result<Observation> {
  let created_at = Utc::now().timestamp_millis();
  let id = transaction.query_row(
    "
      insert into observations(task_id, text, created_at)
      values (?1, ?2, ?3) returning id
      ",
    params![task_id, text, created_at],
    |row| row.get(0),
  )?;
  materialize(ObservationRow {
    id,
    task_id,
    text: text.to_owned(),
    created_at,
  })
}

pub fn after(
  transaction: &Transaction<'_>,
  observation_id: i64,
  task_id: Option<i64>,
) -> Result<Vec<Observation>> {
  let mut statement = transaction.prepare(
    "
      select id, task_id, text, created_at
      from observations
      where id > ?1 and (?2 is null or task_id is null or task_id = ?2)
      order by id
      ",
  )?;
  let rows = statement.query_map(params![observation_id, task_id], observation_row)?;
  rows.map(|row| materialize(row?)).collect()
}

fn observation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ObservationRow> {
  Ok(ObservationRow {
    id: row.get("id")?,
    task_id: row.get("task_id")?,
    text: row.get("text")?,
    created_at: row.get("created_at")?,
  })
}

fn materialize(row: ObservationRow) -> Result<Observation> {
  let created_at = DateTime::from_timestamp_millis(row.created_at)
    .context("observation created_at is outside the supported range")?;
  Observation::new(row.id, row.task_id, row.text, created_at)
}

#[cfg(test)]
mod tests;
