use anyhow::Result;
use chrono::Utc;
use rusqlite::{Transaction, params};

use crate::domain::Calibration;

#[allow(clippy::too_many_arguments)]
pub fn create(
  transaction: &Transaction<'_>,
  task_id: i64,
  predicted_files: i64,
  predicted_lines: i64,
  actual_files: i64,
  actual_lines: i64,
  wall_seconds: Option<f64>,
  context_size_start: i64,
  context_size_end: i64,
) -> Result<Calibration> {
  let created_at = Utc::now();
  let id = transaction.query_row(
    "
      insert into calibrations(
        task_id, predicted_files, predicted_lines, actual_files, actual_lines,
        wall_seconds, created_at, context_size_start, context_size_end
      ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
      returning id
      ",
    params![
      task_id,
      predicted_files,
      predicted_lines,
      actual_files,
      actual_lines,
      wall_seconds,
      created_at.timestamp_millis(),
      context_size_start,
      context_size_end,
    ],
    |row| row.get(0),
  )?;
  let calibration = Calibration::new(
    id,
    task_id,
    predicted_files,
    predicted_lines,
    actual_files,
    actual_lines,
    wall_seconds,
    created_at,
    context_size_start,
    context_size_end,
  )?;
  Ok(calibration)
}

#[cfg(test)]
mod tests;
