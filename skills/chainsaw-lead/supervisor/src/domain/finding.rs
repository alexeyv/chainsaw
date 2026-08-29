use std::fmt;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use super::{require_nonblank, require_positive};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindingVerdict {
  Task,
  Dropped,
}

impl FindingVerdict {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Task => "task",
      Self::Dropped => "dropped",
    }
  }
}

impl fmt::Display for FindingVerdict {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(self.as_str())
  }
}

impl TryFrom<&str> for FindingVerdict {
  type Error = anyhow::Error;

  fn try_from(value: &str) -> Result<Self> {
    match value {
      "task" => Ok(Self::Task),
      "dropped" => Ok(Self::Dropped),
      value => bail!("unknown finding verdict {value:?}"),
    }
  }
}

/// A tracked concern requiring a verdict and reason before it is resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
  id: i64,
  task_id: i64,
  description: String,
  verdict: Option<FindingVerdict>,
  verdict_reason: Option<String>,
  fix_task_id: Option<i64>,
  created_at: DateTime<Utc>,
  resolved_at: Option<DateTime<Utc>>,
}

impl Finding {
  pub fn registered(
    id: i64,
    task_id: i64,
    description: String,
    created_at: DateTime<Utc>,
  ) -> Result<Self> {
    Self::from_record(id, task_id, description, None, None, None, created_at, None)
  }

  #[allow(clippy::too_many_arguments)]
  pub(crate) fn from_record(
    id: i64,
    task_id: i64,
    description: String,
    verdict: Option<FindingVerdict>,
    verdict_reason: Option<String>,
    fix_task_id: Option<i64>,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
  ) -> Result<Self> {
    require_positive("id", id)?;
    require_positive("task_id", task_id)?;
    require_nonblank("description", &description)?;
    if let Some(verdict_reason) = &verdict_reason {
      require_nonblank("verdict_reason", verdict_reason)?;
    }
    if let Some(fix_task_id) = fix_task_id {
      require_positive("fix_task_id", fix_task_id)?;
    }
    match (verdict, verdict_reason.as_ref(), fix_task_id, resolved_at) {
      (None, None, None, None) => {}
      (Some(FindingVerdict::Task), Some(_), Some(_), Some(_)) => {}
      (Some(FindingVerdict::Dropped), Some(_), None, Some(_)) => {}
      (Some(FindingVerdict::Task), _, None, _) => {
        bail!("task verdict requires a fix_task_id")
      }
      (Some(FindingVerdict::Dropped), _, Some(_), _) => {
        bail!("dropped verdict cannot have a fix_task_id")
      }
      (Some(_), None, _, _) => bail!("resolved finding requires a verdict_reason"),
      (Some(_), Some(_), _, None) => bail!("resolved finding requires a resolved_at"),
      (None, _, _, _) => bail!("unresolved finding cannot have resolution fields"),
    }

    Ok(Self {
      id,
      task_id,
      description,
      verdict,
      verdict_reason,
      fix_task_id,
      created_at,
      resolved_at,
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn task_id(&self) -> i64 {
    self.task_id
  }

  pub fn description(&self) -> &str {
    &self.description
  }

  pub fn verdict(&self) -> Option<FindingVerdict> {
    self.verdict
  }

  pub fn verdict_reason(&self) -> Option<&str> {
    self.verdict_reason.as_deref()
  }

  pub fn fix_task_id(&self) -> Option<i64> {
    self.fix_task_id
  }

  pub fn created_at(&self) -> DateTime<Utc> {
    self.created_at
  }

  pub fn resolved_at(&self) -> Option<DateTime<Utc>> {
    self.resolved_at
  }

  pub fn is_resolved(&self) -> bool {
    self.verdict.is_some()
  }
}

#[cfg(test)]
mod tests;
