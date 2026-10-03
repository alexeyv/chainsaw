//! The agent a session runs: what flags it launches with, and where and how
//! its transcript is read. A `SessionRuntime` launches the CLI an `AgentKind`
//! names and drives the terminal; the agent reads what the process in it
//! wrote. The implementations live in `infra`, one per `AgentKind`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;

use super::{SessionKind, SessionRuntime, StartSession, StartedSession, Transcript};

/// Where a sent prompt is in the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptState {
  /// Not in the transcript yet.
  Unseen,
  /// Taken up as the current turn.
  Started,
  /// Waiting in the session's queue behind the current turn.
  Queued,
}

/// When an agent writes a prompt to its transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptEcho {
  /// As it takes the prompt. One still unseen after a while was lost, and can
  /// be sent again.
  OnTake,
  /// Only with its first reply. One still unseen may be at work, and a second
  /// send would be a second prompt; sooner than the reply, only the session
  /// going busy tells that it was taken.
  WithReply,
}

/// A session its agent has started: what the runtime knows it by, and the
/// transcript the agent has begun writing.
#[derive(Debug)]
pub struct Launched {
  pub started: StartedSession,
  pub transcript: PathBuf,
}

pub trait Agent {
  /// Starts a session through `runtime` with `prompt` as its first prompt,
  /// and returns once the agent has begun writing its transcript, or fails
  /// when it has not within a minute.
  fn start(
    &self,
    runtime: &dyn SessionRuntime,
    session: StartSession<'_>,
    prompt: &str,
  ) -> Result<Launched>;

  /// The executable a session of this kind runs, as the agent's CLI is
  /// installed on PATH.
  fn program(&self) -> &'static str;

  /// The flags a session of this kind launches with when settings name none.
  fn default_args(&self, kind: SessionKind) -> String;

  /// The prompt that asks a session to compact its context.
  fn compact_prompt(&self) -> &'static str;

  /// The flags that make a new session take `id` as its own, for an agent
  /// that accepts one. None when the agent names its sessions itself, and the
  /// id has to be read from the transcript the session starts writing.
  fn session_id_args(&self, id: &str) -> Option<Vec<String>>;

  /// The id of the newest session this agent started in `run_dir` at or after
  /// `since`, once it has written its transcript.
  fn session_started_since(&self, canonical_run_dir: &Path, since: SystemTime) -> Option<String>;

  /// The transcript of a session started in `run_dir`, or None until it exists.
  fn transcript(&self, canonical_run_dir: &Path, external_session_id: &str) -> Option<PathBuf>;

  /// The session's transcript at `path`, read in this agent's format, or
  /// None when there is no file there.
  fn open_transcript(&self, path: &Path) -> Option<Box<dyn Transcript>>;

  /// When this agent writes a prompt it was sent to its transcript, and so
  /// what an unseen prompt means.
  fn prompt_echo(&self) -> PromptEcho {
    PromptEcho::OnTake
  }
}
