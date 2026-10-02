use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Transaction, params};

use crate::domain::{RunEvent, RunEventKind};

struct RunEventRow {
  id: i64,
  kind: String,
  detail: String,
  created_at: i64,
}

pub fn create(transaction: &Transaction<'_>, kind: RunEventKind, detail: &str) -> Result<RunEvent> {
  let created_at = Utc::now().timestamp_millis();
  let id = transaction.query_row(
    "
      insert into run_events(kind, detail, created_at)
      values (?1, ?2, ?3) returning id
      ",
    params![kind.as_str(), detail, created_at],
    |row| row.get(0),
  )?;
  materialize(RunEventRow {
    id,
    kind: kind.as_str().to_owned(),
    detail: detail.to_owned(),
    created_at,
  })
}

/// The newest `limit` events of the given kinds, newest first.
pub fn recent(
  transaction: &Transaction<'_>,
  kinds: &[RunEventKind],
  limit: usize,
) -> Result<Vec<RunEvent>> {
  let placeholders = vec!["?"; kinds.len()].join(",");
  let mut statement = transaction.prepare(&format!(
    "
      select id, kind, detail, created_at
      from run_events
      where kind in ({placeholders})
      order by id desc
      limit {limit}
      "
  ))?;
  let names = kinds.iter().map(|kind| kind.as_str()).collect::<Vec<_>>();
  let rows = statement.query_map(rusqlite::params_from_iter(names), run_event_row)?;
  rows.map(|row| materialize(row?)).collect()
}

fn run_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunEventRow> {
  Ok(RunEventRow {
    id: row.get("id")?,
    kind: row.get("kind")?,
    detail: row.get("detail")?,
    created_at: row.get("created_at")?,
  })
}

fn materialize(row: RunEventRow) -> Result<RunEvent> {
  let kind = RunEventKind::try_from(row.kind.as_str())?;
  let created_at = DateTime::from_timestamp_millis(row.created_at)
    .context("run event created_at is outside the supported range")?;
  RunEvent::new(row.id, kind, row.detail, created_at)
}

#[cfg(test)]
mod tests;
