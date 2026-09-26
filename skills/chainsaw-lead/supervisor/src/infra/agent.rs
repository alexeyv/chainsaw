//! The agent a session runs: which coding CLI sits in the terminal, what
//! flags it launches with, and where and how its transcript is read. A
//! session runtime drives the terminal; the agent reads what the process in
//! it wrote. Every session runs Claude Code today.

use std::path::{Path, PathBuf};

use regex::Regex;

use super::session_runtime::SessionKind;
use crate::domain::Session;

mod claude;

pub use claude::Claude;

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

pub trait Agent {
  /// The agent's canonical executable name, as a runtime launches it.
  fn name(&self) -> &'static str;

  /// The flags a session of this kind launches with when settings name none.
  fn default_args(&self, kind: SessionKind) -> String;

  /// The prompt that asks a session to compact its context.
  fn compact_prompt(&self) -> &'static str;

  /// The transcript of a session started in `run_dir`, or None until it exists.
  fn transcript(&self, canonical_run_dir: &Path, external_session_id: &str) -> Option<PathBuf>;

  /// Context the session held at its latest turn.
  fn context_size(&self, transcript: &Path) -> u64;

  /// Context the session held at its last turn before `offset`.
  fn context_before(&self, transcript: &Path, offset: u64) -> u64;

  /// The largest context the session held between `start` and `end`, or to
  /// the end of the transcript.
  fn context_peak(&self, transcript: &Path, start: u64, end: Option<u64>) -> u64;

  /// The state of a prompt opening with `prompt`, sent after `offset`.
  fn prompt_state(&self, transcript: &Path, offset: u64, prompt: &str) -> PromptState;

  /// The last text the agent said, if it has said anything.
  fn latest_assistant_text(&self, transcript: &Path) -> Option<String>;

  /// Whether anything the agent said or did mentions `text`.
  fn output_mentions(&self, transcript: &Path, text: &str) -> bool;

  /// Commit ids recorded from `offset` on, as `[branch sha]` in git's own
  /// commit output. Every agent shows the shell what git printed, so this
  /// reads the transcript as text.
  fn commits_in_log(&self, transcript: &Path, offset: u64) -> Vec<String> {
    let Ok(text) = read_lossy(transcript, offset, None) else {
      return Vec::new();
    };
    let pattern = Regex::new(r"\[[\w/.-]+ ([0-9a-f]{7,40})\]").expect("valid commit regex");
    pattern
      .captures_iter(&text)
      .map(|capture| capture[1].to_owned())
      .collect()
  }
}

/// The agent a session of this kind launches with.
pub fn for_role(_kind: SessionKind) -> &'static dyn Agent {
  &Claude
}

/// The agent behind a session already started.
pub fn for_session(_session: &Session) -> &'static dyn Agent {
  &Claude
}

/// The transcript between two byte offsets, or to its end, tolerating a cut
/// through a multibyte character at either end.
fn read_lossy(path: &Path, start: u64, end: Option<u64>) -> std::io::Result<String> {
  use std::io::{Read, Seek, SeekFrom};

  let mut file = std::fs::File::open(path)?;
  file.seek(SeekFrom::Start(start))?;
  let mut bytes = Vec::new();
  match end {
    Some(end) => {
      file
        .take(end.saturating_sub(start))
        .read_to_end(&mut bytes)?;
    }
    None => {
      file.read_to_end(&mut bytes)?;
    }
  }
  Ok(String::from_utf8_lossy(&bytes).into_owned())
}
