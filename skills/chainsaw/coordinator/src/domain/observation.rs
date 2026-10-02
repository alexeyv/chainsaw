use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use super::require_positive;

/// Informational chronological context that requires no response.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
  id: i64,
  task_id: Option<i64>,
  text: String,
  created_at: DateTime<Utc>,
}

impl Observation {
  pub fn new(
    id: i64,
    task_id: Option<i64>,
    text: String,
    created_at: DateTime<Utc>,
  ) -> Result<Self> {
    require_positive("id", id)?;
    if let Some(task_id) = task_id {
      require_positive("task_id", task_id)?;
    }
    if text.trim().is_empty() {
      bail!("text cannot be blank");
    }
    Ok(Self {
      id,
      task_id,
      text,
      created_at,
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn task_id(&self) -> Option<i64> {
    self.task_id
  }

  pub fn text(&self) -> &str {
    &self.text
  }

  pub fn created_at(&self) -> DateTime<Utc> {
    self.created_at
  }
}

#[cfg(test)]
mod tests;
