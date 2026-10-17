//! A session's transcript, as its agent reads it: the file the agent placed
//! at launch. The implementation lives in `infra`; an `Agent` hands out one
//! in its own format.

use std::path::Path;

use anyhow::Result;

use super::{ContextSize, PromptState};

pub trait Transcript {
  /// Where the agent placed it at launch.
  fn path(&self) -> &Path;

  /// Fails unless the file is on disk and can be read.
  fn check_readable(&self) -> Result<()>;

  /// Bytes written so far.
  fn size(&self) -> Result<u64>;

  /// Context the session held at its latest turn.
  fn context_size(&self) -> Result<ContextSize>;

  /// Context the session held at its last turn before `offset`.
  fn context_before(&self, offset: u64) -> Result<ContextSize>;

  /// The largest context the session held between `from_offset` and
  /// `to_offset`, or to the end of the transcript.
  fn context_peak(&self, from_offset: u64, to_offset: Option<u64>) -> Result<ContextSize>;

  /// The state of a prompt opening with `prompt`, sent after `offset`.
  fn prompt_state(&self, offset: u64, prompt: &str) -> Result<PromptState>;

  /// The last text the agent said, if it has said anything.
  fn latest_assistant_text(&self) -> Result<Option<String>>;

  /// Commit ids the session may have made from `offset` on, given `head`,
  /// where the branch stands now. An agent whose transcript shows what git
  /// printed reads them from it; one whose transcript keeps no tool output
  /// can only name HEAD.
  fn commit_candidates(&self, offset: u64, head: &str) -> Result<Vec<String>>;
}
