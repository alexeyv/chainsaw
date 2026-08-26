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
mod tests {
  use chrono::{DateTime, Utc};

  use super::Calibration;

  fn timestamp(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).unwrap()
  }

  fn calibration_with(
    id: i64,
    task_id: i64,
    wall_seconds: Option<f64>,
    values: [i64; 6],
  ) -> anyhow::Result<Calibration> {
    Calibration::new(
      id,
      task_id,
      values[0],
      values[1],
      values[2],
      values[3],
      wall_seconds,
      timestamp(1_700_000_000),
      values[4],
      values[5],
    )
  }

  #[test]
  fn constructor_exposes_every_field_without_mutators() {
    let created_at = timestamp(1_700_000_000);
    let calibration =
      Calibration::new(3, 7, 2, 20, 4, 35, Some(12.5), created_at, 100, 900).unwrap();

    assert_eq!(calibration.id(), 3);
    assert_eq!(calibration.task_id(), 7);
    assert_eq!(calibration.predicted_files(), 2);
    assert_eq!(calibration.predicted_lines(), 20);
    assert_eq!(calibration.actual_files(), 4);
    assert_eq!(calibration.actual_lines(), 35);
    assert_eq!(calibration.wall_seconds(), Some(12.5));
    assert_eq!(calibration.created_at(), created_at);
    assert_eq!(calibration.context_size_start(), 100);
    assert_eq!(calibration.context_size_end(), 900);
  }

  #[test]
  fn constructor_requires_a_positive_id() {
    for id in [i64::MIN, -1, 0] {
      let error = calibration_with(id, 7, None, [0; 6]).unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn constructor_requires_a_positive_task_id() {
    for task_id in [i64::MIN, -1, 0] {
      let error = calibration_with(1, task_id, None, [0; 6]).unwrap_err();
      assert_eq!(error.to_string(), "task_id must be positive");
    }
  }

  #[test]
  fn constructor_requires_nonnegative_measurements() {
    let fields = [
      "predicted_files",
      "predicted_lines",
      "actual_files",
      "actual_lines",
      "context_size_start",
      "context_size_end",
    ];
    for (index, field) in fields.into_iter().enumerate() {
      let mut values = [0; 6];
      values[index] = -1;
      let error = calibration_with(1, 7, None, values).unwrap_err();
      assert_eq!(error.to_string(), format!("{field} cannot be negative"));
    }
  }

  #[test]
  fn constructor_requires_context_to_end_at_or_after_its_start() {
    let error = calibration_with(1, 7, None, [0, 0, 0, 0, 2, 1]).unwrap_err();
    assert_eq!(
      error.to_string(),
      "context_size_end cannot precede context_size_start"
    );
  }

  #[test]
  fn constructor_allows_an_absent_wall_time_but_rejects_invalid_values() {
    assert!(calibration_with(1, 7, None, [0; 6]).is_ok());
    assert!(calibration_with(1, 7, Some(0.0), [0; 6]).is_ok());

    for wall_seconds in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
      let error = calibration_with(1, 7, Some(wall_seconds), [0; 6]).unwrap_err();
      assert_eq!(
        error.to_string(),
        "wall_seconds must be finite and nonnegative"
      );
    }
  }
}
