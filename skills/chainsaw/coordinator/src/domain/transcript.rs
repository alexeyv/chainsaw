//! A session's transcript, as its agent reads it. Only a transcript on disk
//! can be opened, so there is no asking one whether it exists. The
//! implementation lives in `infra`; an `Agent` opens one in its own format.

use std::path::Path;

use super::{ContextSize, PromptState};

pub trait Transcript {
  /// Where the agent writes it.
  fn path(&self) -> &Path;

  /// Bytes written so far.
  fn size(&self) -> u64;

  /// Context the session held at its latest turn.
  fn context_size(&self) -> ContextSize;

  /// Context the session held at its last turn before `offset`.
  fn context_before(&self, offset: u64) -> ContextSize;

  /// The largest context the session held between `start` and `end`, or to
  /// the end of the transcript.
  fn context_peak(&self, start: u64, end: Option<u64>) -> ContextSize;

  /// The state of a prompt opening with `prompt`, sent after `offset`.
  fn prompt_state(&self, offset: u64, prompt: &str) -> PromptState;

  /// The last text the agent said, if it has said anything.
  fn latest_assistant_text(&self) -> Option<String>;

  /// Whether anything the agent said or did mentions `text`.
  fn output_mentions(&self, text: &str) -> bool;

  /// Commit ids the session may have made from `offset` on, given `head`,
  /// where the branch stands now. An agent whose transcript shows what git
  /// printed reads them from it; one whose transcript keeps no tool output
  /// can only name HEAD.
  fn commit_candidates(&self, offset: u64, head: &str) -> Vec<String>;
}
