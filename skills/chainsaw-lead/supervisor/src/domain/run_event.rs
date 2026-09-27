use std::fmt;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use strum::{EnumIter, IntoEnumIterator};

use super::{require_nonblank, require_positive};

/// What the supervisor did to the run: one entry in its operational journal.
/// The name is what the row stores and what `state` prints; `as_str` is the
/// only place a name is spelled out, and parsing iterates the enum against it.
#[derive(Clone, Copy, Debug, EnumIter, Eq, PartialEq)]
pub enum RunEventKind {
  Launch,
  PromptQueued,
  PromptTaken,
  PromptFailed,
  PromptUnreachable,
  Dispatch,
  DispatchFailed,
  Committed,
  ForcedCommit,
  ForcedCommentary,
  CommentaryWake,
  CommentaryDelivered,
  Accepted,
  Aborted,
  AbortInterrupt,
  AbortUnreachable,
  Kick,
  Compact,
  StopLead,
  Stop,
  DaemonStart,
  DaemonExit,
  TranscriptMissing,
  TranscriptFound,
}

impl RunEventKind {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Launch => "launch",
      Self::PromptQueued => "prompt-queued",
      Self::PromptTaken => "prompt-taken",
      Self::PromptFailed => "prompt-failed",
      Self::PromptUnreachable => "prompt-unreachable",
      Self::Dispatch => "dispatch",
      Self::DispatchFailed => "dispatch-failed",
      Self::Committed => "committed",
      Self::ForcedCommit => "forced-commit",
      Self::ForcedCommentary => "forced-commentary",
      Self::CommentaryWake => "commentary-wake",
      Self::CommentaryDelivered => "commentary-delivered",
      Self::Accepted => "accepted",
      Self::Aborted => "aborted",
      Self::AbortInterrupt => "abort-interrupt",
      Self::AbortUnreachable => "abort-unreachable",
      Self::Kick => "kick",
      Self::Compact => "compact",
      Self::StopLead => "stop-lead",
      Self::Stop => "stop",
      Self::DaemonStart => "daemon-start",
      Self::DaemonExit => "daemon-exit",
      Self::TranscriptMissing => "transcript-missing",
      Self::TranscriptFound => "transcript-found",
    }
  }
}

impl TryFrom<&str> for RunEventKind {
  type Error = anyhow::Error;

  fn try_from(value: &str) -> Result<Self> {
    match Self::iter().find(|kind| kind.as_str() == value) {
      Some(kind) => Ok(kind),
      None => bail!("unknown run event kind {value:?}"),
    }
  }
}

impl fmt::Display for RunEventKind {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(self.as_str())
  }
}

/// Something the supervisor did to the run, kept so an operator or the lead
/// can see what happened. Nothing is decided from it.
#[derive(Clone, Debug, PartialEq)]
pub struct RunEvent {
  id: i64,
  kind: RunEventKind,
  detail: String,
  created_at: DateTime<Utc>,
}

impl RunEvent {
  pub fn new(
    id: i64,
    kind: RunEventKind,
    detail: String,
    created_at: DateTime<Utc>,
  ) -> Result<Self> {
    require_positive("id", id)?;
    require_nonblank("detail", &detail)?;
    Ok(Self {
      id,
      kind,
      detail,
      created_at,
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn kind(&self) -> RunEventKind {
    self.kind
  }

  pub fn detail(&self) -> &str {
    &self.detail
  }

  pub fn created_at(&self) -> DateTime<Utc> {
    self.created_at
  }
}

#[cfg(test)]
mod tests;
