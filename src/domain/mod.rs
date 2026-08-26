mod calibration;
mod finding;
mod observation;
mod task;
mod task_event;

use anyhow::{Result, bail};

pub use calibration::Calibration;
pub use finding::{Finding, FindingVerdict};
pub use observation::Observation;
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
