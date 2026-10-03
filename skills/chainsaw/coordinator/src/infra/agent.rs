//! The agents a session can run: Claude Code, OpenAI Codex or the Cursor
//! Agent CLI. Which one a session runs is recorded on the session. Each reads
//! its own transcript format, and what reading a JSONL transcript takes is
//! shared here, along with the transcript an agent opens on its format.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use regex::Regex;
use serde_json::Value;

use super::transcript_monitor::transcript_size;
use crate::domain::{
  Agent, AgentKind, ContextSize, Launched, PromptState, SessionRuntime, StartSession, Transcript,
};

/// How long a new session has to begin its transcript before its start fails.
const TRANSCRIPT_TIMEOUT: Duration = Duration::from_secs(60);

/// How often a start looks for the transcript it is waiting on.
const TRANSCRIPT_POLL: Duration = Duration::from_millis(250);

mod claude;
mod codex;
mod cursor;

pub use claude::Claude;
pub use codex::Codex;
pub use cursor::Cursor;

/// The implementation of a named agent.
pub fn implementing(kind: AgentKind) -> &'static dyn Agent {
  match kind {
    AgentKind::Claude => &Claude,
    AgentKind::Codex => &Codex,
    AgentKind::Cursor => &Cursor,
  }
}

/// How an agent reads a transcript in its own format, handed the file.
trait TranscriptFormat: Sync {
  fn context_size(&self, transcript: &Path) -> ContextSize;
  fn context_before(&self, transcript: &Path, offset: u64) -> ContextSize;
  fn context_peak(&self, transcript: &Path, start: u64, end: Option<u64>) -> ContextSize;
  fn prompt_state(&self, transcript: &Path, offset: u64, prompt: &str) -> PromptState;
  fn latest_assistant_text(&self, transcript: &Path) -> Option<String>;
  fn output_mentions(&self, transcript: &Path, text: &str) -> bool;
  fn commit_candidates(&self, transcript: &Path, offset: u64, head: &str) -> Vec<String>;
}

/// A transcript file, read in its agent's format.
struct TranscriptFile {
  format: &'static dyn TranscriptFormat,
  path: PathBuf,
}

/// The transcript at `path` in `format`, when there is a file there.
fn open(format: &'static dyn TranscriptFormat, path: &Path) -> Option<Box<dyn Transcript>> {
  path.is_file().then(|| {
    Box::new(TranscriptFile {
      format,
      path: path.to_owned(),
    }) as Box<dyn Transcript>
  })
}

impl Transcript for TranscriptFile {
  fn path(&self) -> &Path {
    &self.path
  }

  fn size(&self) -> u64 {
    transcript_size(&self.path)
  }

  fn context_size(&self) -> ContextSize {
    self.format.context_size(&self.path)
  }

  fn context_before(&self, offset: u64) -> ContextSize {
    self.format.context_before(&self.path, offset)
  }

  fn context_peak(&self, start: u64, end: Option<u64>) -> ContextSize {
    self.format.context_peak(&self.path, start, end)
  }

  fn prompt_state(&self, offset: u64, prompt: &str) -> PromptState {
    self.format.prompt_state(&self.path, offset, prompt)
  }

  fn latest_assistant_text(&self) -> Option<String> {
    self.format.latest_assistant_text(&self.path)
  }

  fn output_mentions(&self, text: &str) -> bool {
    self.format.output_mentions(&self.path, text)
  }

  fn commit_candidates(&self, offset: u64, head: &str) -> Vec<String> {
    self.format.commit_candidates(&self.path, offset, head)
  }
}

/// Starts the session with `prompt` last on its command line, after `--` so
/// no option that takes several values swallows it, and waits for the
/// transcript the agent opens as it takes the prompt.
fn start_with_prompt(
  agent: &dyn Agent,
  runtime: &dyn SessionRuntime,
  session: StartSession<'_>,
  prompt: &str,
) -> Result<Launched> {
  let mut args = session.args.to_vec();
  args.extend(["--".to_owned(), prompt.to_owned()]);
  let run_dir = session.run_dir;
  let started = runtime.start(StartSession {
    args: &args,
    ..session
  })?;
  let transcript = transcript_within(agent, run_dir, &started.external_id, TRANSCRIPT_TIMEOUT)?;
  Ok(Launched {
    started,
    transcript,
  })
}

/// The transcript of the session `external_id` once the agent has begun it,
/// or why it did not within `timeout`.
fn transcript_within(
  agent: &dyn Agent,
  canonical_run_dir: &Path,
  external_id: &str,
  timeout: Duration,
) -> Result<PathBuf> {
  let started = Instant::now();
  loop {
    if let Some(transcript) = agent.transcript(canonical_run_dir, external_id) {
      return Ok(transcript);
    }
    if started.elapsed() >= timeout {
      bail!(
        "{} session {external_id} wrote no transcript within {} seconds",
        agent.program(),
        timeout.as_secs()
      );
    }
    thread::sleep(TRANSCRIPT_POLL);
  }
}

/// The commits git printed into the transcript from `offset` on, as
/// `[branch sha]` in its own commit output.
fn commits_printed(transcript: &Path, offset: u64) -> Vec<String> {
  let Ok(text) = read_lossy(transcript, offset, None) else {
    return Vec::new();
  };
  let pattern = Regex::new(r"\[[\w/.-]+ ([0-9a-f]{7,40})\]").expect("valid commit regex");
  pattern
    .captures_iter(&text)
    .map(|capture| capture[1].to_owned())
    .collect()
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

/// Every entry from `offset` on that parses as JSON.
fn entries(path: &Path, offset: u64) -> Vec<Value> {
  let Ok(text) = read_lossy(path, offset, None) else {
    return Vec::new();
  };
  text
    .lines()
    .filter_map(|line| serde_json::from_str(line).ok())
    .collect()
}

/// The text of a message's content: its blocks' text joined, or the string
/// itself.
fn text_of(content: &Value) -> String {
  match content {
    Value::Array(blocks) => blocks
      .iter()
      .filter_map(|block| block.get("text").and_then(Value::as_str))
      .collect::<Vec<_>>()
      .join(" "),
    Value::String(text) => text.clone(),
    other => other.to_string(),
  }
}

#[cfg(test)]
mod tests;
