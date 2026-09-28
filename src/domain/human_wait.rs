use anyhow::{Result, bail};
use chrono::{DateTime, Duration, Utc};

use super::require_positive;

/// One interval the run spent waiting on the human. At most one is open at a
/// time; an open one has no end yet.
#[derive(Clone, Debug, PartialEq)]
pub struct HumanWait {
  id: i64,
  started: DateTime<Utc>,
  ended: Option<DateTime<Utc>>,
}

impl HumanWait {
  pub fn new(id: i64, started: DateTime<Utc>, ended: Option<DateTime<Utc>>) -> Result<Self> {
    require_positive("id", id)?;
    if ended.is_some_and(|ended| ended < started) {
      bail!("ended cannot precede started");
    }
    Ok(Self { id, started, ended })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn started(&self) -> DateTime<Utc> {
    self.started
  }

  pub fn ended(&self) -> Option<DateTime<Utc>> {
    self.ended
  }

  pub fn is_open(&self) -> bool {
    self.ended.is_none()
  }

  /// How long the wait lasted, or has lasted as seen at `now` while open.
  /// Never negative, so a clock that runs behind the start reads as nothing.
  pub fn duration(&self, now: DateTime<Utc>) -> Duration {
    (self.ended.unwrap_or(now) - self.started).max(Duration::zero())
  }
}

#[cfg(test)]
mod tests;
