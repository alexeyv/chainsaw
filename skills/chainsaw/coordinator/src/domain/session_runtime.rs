//! What a run's sessions live in: a terminal multiplexer that opens a pane or
//! tab per session, launches the agent in it, and drives it by name. The
//! agent behind the pane is the `Agent`'s business. The implementations live
//! in `infra`; the run owns one and every `Session` borrows it.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use super::{AgentKind, SessionKind};

pub struct StartSession<'a> {
  pub id: &'a str,
  pub run_dir: &'a Path,
  pub kind: SessionKind,
  /// Which coding CLI to launch in the pane.
  pub agent: AgentKind,
  /// The agent's flags, verbatim.
  pub args: &'a [String],
}

#[derive(Debug)]
pub struct StartedSession {
  pub external_id: String,
  pub pane_id: String,
  pub tab_id: String,
}

/// What a session is doing, as far as its runtime can tell. Herdr and Orca
/// each spell this their own way; the domain compares against this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
  /// At its prompt, or finished: it would take a prompt now.
  Idle,
  /// Mid-turn, including waiting on a permission.
  Busy,
  /// The runtime cannot tell.
  Unknown,
}

pub trait SessionRuntime {
  fn start(&self, session: StartSession<'_>) -> Result<StartedSession>;
  /// What the session named is doing, or None when the runtime has no such
  /// session.
  fn status(&self, session_id: &str) -> Result<Option<SessionStatus>>;
  fn prompt(&self, session_id: &str, text: &str) -> Result<()>;
  fn interrupt(&self, session_id: &str) -> Result<()>;
  fn wait(&self, session_id: &str, timeout: Duration) -> Result<()>;
}
