use anyhow::Result;
use chrono::{DateTime, Utc};

use super::{require_nonblank, require_nonnegative, require_positive};

/// One prompt the supervisor sent to a `Session`, and how the send fared: how
/// many times it went out and when the session's transcript first showed it.
#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
  id: i64,
  session_id: i64,
  text: String,
  sent_at: DateTime<Utc>,
  seen_at: Option<DateTime<Utc>>,
  attempts: i64,
}

impl Prompt {
  pub fn new(
    id: i64,
    session_id: i64,
    text: String,
    sent_at: DateTime<Utc>,
    seen_at: Option<DateTime<Utc>>,
    attempts: i64,
  ) -> Result<Self> {
    require_positive("id", id)?;
    require_positive("session_id", session_id)?;
    require_nonblank("text", &text)?;
    require_nonnegative("attempts", attempts)?;
    Ok(Self {
      id,
      session_id,
      text,
      sent_at,
      seen_at,
      attempts,
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn session_id(&self) -> i64 {
    self.session_id
  }

  pub fn text(&self) -> &str {
    &self.text
  }

  pub fn sent_at(&self) -> DateTime<Utc> {
    self.sent_at
  }

  /// When the session's transcript first showed the prompt; None until it has.
  pub fn seen_at(&self) -> Option<DateTime<Utc>> {
    self.seen_at
  }

  /// How many times the prompt has been sent.
  pub fn attempts(&self) -> i64 {
    self.attempts
  }
}

#[cfg(test)]
mod tests;
