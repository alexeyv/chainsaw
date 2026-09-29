//! The agent a session runs: what flags it launches with, and where and how
//! its transcript is read. A `SessionRuntime` launches the CLI an `AgentKind`
//! names and drives the terminal; the agent reads what the process in it
//! wrote. The implementations live in `infra`, one per `AgentKind`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{ContextSize, SessionKind};

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

pub trait Agent {
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

  /// Context the session held at its latest turn.
  fn context_size(&self, transcript: &Path) -> ContextSize;

  /// Context the session held at its last turn before `offset`.
  fn context_before(&self, transcript: &Path, offset: u64) -> ContextSize;

  /// The largest context the session held between `start` and `end`, or to
  /// the end of the transcript.
  fn context_peak(&self, transcript: &Path, start: u64, end: Option<u64>) -> ContextSize;

  /// The state of a prompt opening with `prompt`, sent after `offset`.
  fn prompt_state(&self, transcript: &Path, offset: u64, prompt: &str) -> PromptState;

  /// When this agent writes a prompt it was sent to its transcript, and so
  /// what an unseen prompt means.
  fn prompt_echo(&self) -> PromptEcho {
    PromptEcho::OnTake
  }

  /// The last text the agent said, if it has said anything.
  fn latest_assistant_text(&self, transcript: &Path) -> Option<String>;

  /// Whether anything the agent said or did mentions `text`.
  fn output_mentions(&self, transcript: &Path, text: &str) -> bool;

  /// Commit ids the session may have made from `offset` on, given `head`,
  /// where the branch stands now. An agent whose transcript shows what git
  /// printed reads them from it; one whose transcript keeps no tool output
  /// can only name HEAD.
  fn commit_candidates(&self, transcript: &Path, offset: u64, head: &str) -> Vec<String>;
}
