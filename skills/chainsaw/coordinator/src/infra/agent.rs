//! The agents a session can run: Claude Code, OpenAI Codex or the Cursor
//! Agent CLI. Which one a session runs is recorded on the session. Each reads
//! its own transcript format, and what reading a JSONL transcript takes is
//! shared here; the transcript an agent hands a session is in
//! `transcript_file`.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use regex::Regex;
use serde_json::Value;

use crate::domain::{Agent, AgentKind, Launched, SessionRuntime, StartSession};

/// How long a new session has to begin its transcript before its start fails.
const TRANSCRIPT_TIMEOUT: Duration = Duration::from_secs(60);

/// How often a start looks for the transcript it is waiting on.
const TRANSCRIPT_POLL: Duration = Duration::from_millis(250);

mod claude;
mod codex;
mod cursor;
mod transcript_file;

pub use claude::Claude;
pub use codex::Codex;
pub use cursor::Cursor;
use transcript_file::{TranscriptFormat, transcript_file};

/// The implementation of a named agent.
pub fn implementing(kind: AgentKind) -> &'static dyn Agent {
  match kind {
    AgentKind::Claude => &Claude,
    AgentKind::Codex => &Codex,
    AgentKind::Cursor => &Cursor,
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
  start_with_prompt_within(agent, runtime, session, prompt, TRANSCRIPT_TIMEOUT)
}

/// A session whose agent begins no transcript within `timeout` is abandoned:
/// the runtime closes its pane and forgets it, since nothing the supervisor
/// could drive is in there, and the failure says so.
fn start_with_prompt_within(
  agent: &dyn Agent,
  runtime: &dyn SessionRuntime,
  session: StartSession<'_>,
  prompt: &str,
  timeout: Duration,
) -> Result<Launched> {
  let mut args = session.args.to_vec();
  args.extend(["--".to_owned(), prompt.to_owned()]);
  let run_dir = session.run_dir;
  let session_id = session.id;
  let started = runtime.start(StartSession {
    args: &args,
    ..session
  })?;
  let transcript = match transcript_within(agent, run_dir, &started.external_id, timeout) {
    Ok(transcript) => transcript,
    Err(error) => {
      return Err(match runtime.abandon(session_id, &started.pane_id) {
        Ok(()) => anyhow!("{error}; its pane was closed"),
        Err(abandoning) => anyhow!("{error}; and its pane could not be closed: {abandoning}"),
      });
    }
  };
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
