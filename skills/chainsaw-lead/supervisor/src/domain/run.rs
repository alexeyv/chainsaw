use anyhow::Result;
use chrono::{DateTime, Utc};

/// Live facts about the run as a whole: when a daemon last polled, when the
/// lead asked the run to stop, and when state was last read. There is exactly
/// one per database, seeded with the schema, so every field starts empty.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
  daemon_seen_at: Option<DateTime<Utc>>,
  stop_requested_at: Option<DateTime<Utc>>,
  state_read_at: Option<DateTime<Utc>>,
}

impl Run {
  /// The three facts are independent of one another: a stop may be requested
  /// before any daemon polls, and state may be read before either.
  pub fn new(
    daemon_seen_at: Option<DateTime<Utc>>,
    stop_requested_at: Option<DateTime<Utc>>,
    state_read_at: Option<DateTime<Utc>>,
  ) -> Result<Self> {
    Ok(Self {
      daemon_seen_at,
      stop_requested_at,
      state_read_at,
    })
  }

  pub fn daemon_seen_at(&self) -> Option<DateTime<Utc>> {
    self.daemon_seen_at
  }

  pub fn stop_requested_at(&self) -> Option<DateTime<Utc>> {
    self.stop_requested_at
  }

  pub fn state_read_at(&self) -> Option<DateTime<Utc>> {
    self.state_read_at
  }

  /// A stop stands until the next daemon start clears it.
  pub fn is_stopping(&self) -> bool {
    self.stop_requested_at.is_some()
  }

  /// Whole seconds since a daemon last polled, as seen at `now`; `None` until
  /// one has. Never negative, so a clock that runs behind reads as "just now".
  pub fn seconds_since_daemon_seen(&self, now: DateTime<Utc>) -> Option<i64> {
    self.daemon_seen_at.map(|seen| seconds_since(seen, now))
  }

  /// Whole seconds since state was last read, as seen at `now`; `None` until
  /// it has been. Never negative.
  pub fn seconds_since_state_read(&self, now: DateTime<Utc>) -> Option<i64> {
    self.state_read_at.map(|read| seconds_since(read, now))
  }
}

fn seconds_since(then: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
  (now - then).num_seconds().max(0)
}

#[cfg(test)]
mod tests;
