use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Transaction, params};

use crate::domain::Observation;

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
  materialize(id, task_id, text.to_owned(), created_at)
}

pub fn after(
  db: &Connection,
  observation_id: i64,
  task_id: Option<i64>,
) -> Result<Vec<Observation>> {
  let mut statement = db.prepare(
    "
      select id, task_id, text, created_at
      from observations
      where id > ?1 and (?2 is null or task_id is null or task_id = ?2)
      order by id
      ",
  )?;
  let rows = statement.query_map(params![observation_id, task_id], |row| {
    Ok((
      row.get::<_, i64>(0)?,
      row.get::<_, Option<i64>>(1)?,
      row.get::<_, String>(2)?,
      row.get::<_, i64>(3)?,
    ))
  })?;
  rows
    .map(|row| {
      let (id, task_id, text, created_at) = row?;
      materialize(id, task_id, text, created_at)
    })
    .collect()
}

fn materialize(
  id: i64,
  task_id: Option<i64>,
  text: String,
  created_at: i64,
) -> Result<Observation> {
  let created_at = DateTime::from_timestamp_millis(created_at)
    .context("observation created_at is outside the supported range")?;
  Observation::new(id, task_id, text, created_at)
}

#[cfg(test)]
mod tests;
