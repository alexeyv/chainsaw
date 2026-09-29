//! The agents a session can run: Claude Code, OpenAI Codex or the Cursor
//! Agent CLI. Which one a session runs is recorded on the session. Each reads
//! its own transcript format, and what reading a JSONL transcript takes is
//! shared here.

use std::path::Path;

use regex::Regex;
use serde_json::Value;

use crate::domain::{Agent, AgentKind, Session};

mod claude;
mod codex;
mod cursor;

pub use claude::Claude;
pub use codex::Codex;
pub use cursor::Cursor;

/// The agent behind a session already started: the one its row names.
pub fn for_session(session: &Session) -> &'static dyn Agent {
  implementing(session.agent())
}

/// The implementation of a named agent.
pub fn implementing(kind: AgentKind) -> &'static dyn Agent {
  match kind {
    AgentKind::Claude => &Claude,
    AgentKind::Codex => &Codex,
    AgentKind::Cursor => &Cursor,
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
