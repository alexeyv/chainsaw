use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use super::{require_nonnegative, require_positive};

#[derive(Clone, Debug, PartialEq)]
pub struct Calibration {
  id: i64,
  task_id: i64,
  predicted_files: i64,
  predicted_lines: i64,
  actual_files: i64,
  actual_lines: i64,
  wall_seconds: Option<f64>,
  created_at: DateTime<Utc>,
  context_size_start: i64,
  context_size_end: i64,
}

impl Calibration {
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    id: i64,
    task_id: i64,
    predicted_files: i64,
    predicted_lines: i64,
    actual_files: i64,
    actual_lines: i64,
    wall_seconds: Option<f64>,
    created_at: DateTime<Utc>,
    context_size_start: i64,
    context_size_end: i64,
  ) -> Result<Self> {
    require_positive("id", id)?;
    require_positive("task_id", task_id)?;
    require_nonnegative("predicted_files", predicted_files)?;
    require_nonnegative("predicted_lines", predicted_lines)?;
    require_nonnegative("actual_files", actual_files)?;
    require_nonnegative("actual_lines", actual_lines)?;
    if wall_seconds.is_some_and(|seconds| !seconds.is_finite() || seconds < 0.0) {
      bail!("wall_seconds must be finite and nonnegative");
    }
    require_nonnegative("context_size_start", context_size_start)?;
    require_nonnegative("context_size_end", context_size_end)?;
    if context_size_end < context_size_start {
      bail!("context_size_end cannot precede context_size_start");
    }

    Ok(Self {
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
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn task_id(&self) -> i64 {
    self.task_id
  }

  pub fn predicted_files(&self) -> i64 {
    self.predicted_files
  }

  pub fn predicted_lines(&self) -> i64 {
    self.predicted_lines
  }

  pub fn actual_files(&self) -> i64 {
    self.actual_files
  }

  pub fn actual_lines(&self) -> i64 {
    self.actual_lines
  }

  pub fn wall_seconds(&self) -> Option<f64> {
    self.wall_seconds
  }

  pub fn created_at(&self) -> DateTime<Utc> {
    self.created_at
  }

  pub fn context_size_start(&self) -> i64 {
    self.context_size_start
  }

  pub fn context_size_end(&self) -> i64 {
    self.context_size_end
  }
}

#[cfg(test)]
mod tests;
