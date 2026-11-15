use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

/// Live facts about the run as a whole: when a daemon last polled and which
/// daemon owns the run, when the lead asked the run to stop, when state was
/// last read, and how far the lead has read the observations. There is
/// exactly one per database, seeded with the schema, so every field starts
/// empty.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
  daemon_seen_at: Option<DateTime<Utc>>,
  stop_requested_at: Option<DateTime<Utc>>,
  state_read_at: Option<DateTime<Utc>>,
  observation_cursor: i64,
  daemon_pid: Option<u32>,
}

impl Run {
  /// The facts are independent of one another: a stop may be requested
  /// before any daemon polls, and state may be read before either.
  pub fn new(
    daemon_seen_at: Option<DateTime<Utc>>,
    stop_requested_at: Option<DateTime<Utc>>,
    state_read_at: Option<DateTime<Utc>>,
    observation_cursor: i64,
    daemon_pid: Option<u32>,
  ) -> Result<Self> {
    if observation_cursor < 0 {
      bail!("observation_cursor cannot be negative");
    }
    Ok(Self {
      daemon_seen_at,
      stop_requested_at,
      state_read_at,
      observation_cursor,
      daemon_pid,
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

  /// The cursor the lead's last run-wide poll returned: the observations
  /// before it have been delivered. A continuing lead polls after it.
  pub fn observation_cursor(&self) -> i64 {
    self.observation_cursor
  }

  /// The process id of the daemon that started last, which owns the run; an
  /// earlier daemon still polling has been superseded.
  pub fn daemon_pid(&self) -> Option<u32> {
    self.daemon_pid
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
