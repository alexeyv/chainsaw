//! The terminal multiplexer a run's sessions live in. A runtime opens a pane
//! or tab per session, launches the agent in it, and drives it by name; the
//! agent behind the pane is the `Agent`'s business.

use std::env;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use crate::domain::{AgentKind, Role};

mod herdr;
mod orca;

pub use herdr::HerdrSessionRuntime;
pub use orca::OrcaSessionRuntime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
  Implementer,
  Commentator,
}

impl SessionKind {
  pub fn label(self) -> &'static str {
    self.role().as_str()
  }

  /// The role a session of this kind is recorded with. The lead is never
  /// launched, so it has no kind.
  pub fn role(self) -> Role {
    match self {
      Self::Implementer => Role::Implementer,
      Self::Commentator => Role::Commentator,
    }
  }
}

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

#[derive(Debug)]
pub struct SessionQuery {
  pub external_id: String,
  pub status: String,
}

pub trait SessionRuntime {
  fn start(&self, session: StartSession<'_>) -> Result<StartedSession>;
  fn query(&self, session_id: &str) -> Result<Option<SessionQuery>>;
  fn prompt(&self, session_id: &str, text: &str) -> Result<()>;
  fn interrupt(&self, session_id: &str) -> Result<()>;
  fn wait(&self, session_id: &str, timeout: Duration) -> Result<()>;
}

/// The runtime the supervisor was started under: Herdr inside a Herdr pane,
/// Orca inside an Orca terminal. A Herdr pane opened from an Orca terminal
/// sees both and is a Herdr pane. Outside both, Herdr, which then refuses to
/// start a session. The tests put a `herdr` of their own on PATH.
pub fn from_environment(run_dir: &Path) -> Result<Box<dyn SessionRuntime>> {
  Ok(
    match runtime_named_by(
      env::var_os("HERDR_WORKSPACE_ID").is_some(),
      env::var("ORCA_TERMINAL_HANDLE").ok(),
    ) {
      Some(terminal) => Box::new(OrcaSessionRuntime::from_environment(run_dir, terminal)?),
      None => Box::new(HerdrSessionRuntime::from_environment()),
    },
  )
}

/// The Orca terminal to drive sessions from, or None for Herdr.
fn runtime_named_by(inside_herdr: bool, orca_terminal: Option<String>) -> Option<String> {
  orca_terminal.filter(|_| !inside_herdr)
}

#[cfg(test)]
mod tests;
