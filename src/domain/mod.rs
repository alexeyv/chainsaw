mod calibration;
mod finding;
mod observation;
mod session;
mod task;
mod task_event;

#[cfg(test)]
pub(crate) mod test_helpers;

use anyhow::{Result, bail};

pub use calibration::Calibration;
pub use finding::{Finding, FindingVerdict};
pub use observation::Observation;
pub use session::{Role, Session};
pub use task::{Task, TaskState};
pub use task_event::TaskEvent;

fn require_positive(field: &'static str, value: i64) -> Result<()> {
  if value <= 0 {
    bail!("{field} must be positive");
  }
  Ok(())
}

fn require_nonnegative(field: &'static str, value: i64) -> Result<()> {
  if value < 0 {
    bail!("{field} cannot be negative");
  }
  Ok(())
}

fn require_nonblank(field: &'static str, value: &str) -> Result<()> {
  if value.trim().is_empty() {
    bail!("{field} cannot be blank");
  }
  Ok(())
}

fn require_optional_positive(field: &'static str, value: Option<i64>) -> Result<()> {
  match value {
    Some(value) => require_positive(field, value),
    None => Ok(()),
  }
}

fn require_optional_nonnegative(field: &'static str, value: Option<i64>) -> Result<()> {
  match value {
    Some(value) => require_nonnegative(field, value),
    None => Ok(()),
  }
}

fn require_optional_nonblank(field: &'static str, value: Option<&str>) -> Result<()> {
  match value {
    Some(value) => require_nonblank(field, value),
    None => Ok(()),
  }
}
