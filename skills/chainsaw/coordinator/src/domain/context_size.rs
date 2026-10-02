//! How much context a session holds: a token count read from its transcript,
//! or unknown when the agent's transcript records no usage at all.

use std::fmt;

use anyhow::{Result, bail};

/// A context reading. An unknown reading compares with no limit, so it
/// neither exceeds nor stays under one, and it neither is nor becomes a
/// measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContextSize(Option<u64>);

impl ContextSize {
  pub const UNKNOWN: Self = Self(None);

  pub fn tokens(tokens: u64) -> Self {
    Self(Some(tokens))
  }

  /// The count when there is one: how a reading is stored and formatted for
  /// a test.
  pub fn known(self) -> Option<u64> {
    self.0
  }

  /// A reading as a row stores it: null for unknown, and never negative.
  pub fn from_stored(tokens: Option<i64>) -> Result<Self> {
    match tokens {
      Some(tokens) if tokens < 0 => bail!("context size cannot be negative"),
      Some(tokens) => Ok(Self::tokens(tokens.unsigned_abs())),
      None => Ok(Self::UNKNOWN),
    }
  }

  /// The count as a row stores it: null for unknown.
  pub fn stored(self) -> Option<i64> {
    self
      .0
      .map(|tokens| i64::try_from(tokens).unwrap_or(i64::MAX))
  }

  pub fn is_known(self) -> bool {
    self.0.is_some()
  }

  pub fn exceeds(self, limit: u64) -> bool {
    self.0.is_some_and(|tokens| tokens > limit)
  }

  pub fn is_under(self, limit: u64) -> bool {
    self.0.is_some_and(|tokens| tokens < limit)
  }

  /// This reading when it is known, else `other`.
  pub fn or(self, other: Self) -> Self {
    if self.is_known() { self } else { other }
  }

  /// How much the context grew from `base` to this reading: unknown when
  /// either is, and never negative.
  pub fn since(self, base: Self) -> Self {
    Self(
      self
        .0
        .zip(base.0)
        .map(|(end, base)| end.saturating_sub(base)),
    )
  }
}

impl fmt::Display for ContextSize {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.0 {
      Some(tokens) => f.pad(&tokens.to_string()),
      None => f.pad("unknown"),
    }
  }
}

#[cfg(test)]
mod tests;
