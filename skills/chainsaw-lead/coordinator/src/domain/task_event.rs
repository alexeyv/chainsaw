use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use super::require_optional_nonblank;
use super::task::TaskState;

#[derive(Clone, Debug, PartialEq)]
pub struct TaskEvent {
  id: i64,
  state: TaskState,
  reason: Option<String>,
  created_at: DateTime<Utc>,
}

impl TaskEvent {
  pub fn new(
    id: i64,
    state: TaskState,
    reason: Option<String>,
    created_at: DateTime<Utc>,
  ) -> Result<Self> {
    if id <= 0 {
      bail!("id must be positive");
    }
    require_optional_nonblank("reason", reason.as_deref())?;

    Ok(Self {
      id,
      state,
      reason,
      created_at,
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn state(&self) -> TaskState {
    self.state
  }

  /// Why this transition was made, when the caller recorded a reason.
  pub fn reason(&self) -> Option<&str> {
    self.reason.as_deref()
  }

  pub fn created_at(&self) -> DateTime<Utc> {
    self.created_at
  }
}

#[cfg(test)]
mod tests;
