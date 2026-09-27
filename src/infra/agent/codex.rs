//! OpenAI Codex: rollouts are JSONL under `$CODEX_HOME/sessions/<year>/<month>/<day>/`,
//! named `rollout-<started at>-<session id>.jsonl`, one entry per event, with
//! the model's usage on a `token_usage_record` entry after every response.

use std::env;
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{Agent, PromptState, entries, read_lossy, text_of};
use crate::domain::ContextSize;
use crate::infra::session_runtime::SessionKind;

pub struct Codex;

impl Agent for Codex {
  fn program(&self) -> &'static str {
    "codex"
  }

  /// An unattended session edits, builds, tests and commits without asking.
  /// Codex's sandboxes keep `.git` read-only, so under `--sandbox
  /// workspace-write` a `git commit` fails with "Unable to create
  /// .git/index.lock: Operation not permitted" (Codex 0.157.1, 2026-09-26),
  /// and `--full-auto` then stops at the approval that failure raises. The
  /// model stays whatever Codex's own configuration says. The trailing `.`
  /// is the session's first prompt: Codex writes its rollout, and so has a
  /// session id to report, only once a prompt lands.
  fn default_args(&self, _kind: SessionKind) -> String {
    "--dangerously-bypass-approvals-and-sandbox .".to_owned()
  }

  fn compact_prompt(&self) -> &'static str {
    "/compact"
  }

  /// Codex names its own sessions.
  fn session_id_args(&self, _id: &str) -> Option<Vec<String>> {
    None
  }

  fn session_started_since(&self, canonical_run_dir: &Path, since: SystemTime) -> Option<String> {
    newest_rollout_in(&Self::sessions_dir().ok()?, canonical_run_dir, since, 4)
  }

  /// Codex shards rollouts by date rather than by working directory, so the
  /// run directory plays no part: the rollout is whichever file under the
  /// sessions directory is named after the session.
  fn transcript(&self, _canonical_run_dir: &Path, external_session_id: &str) -> Option<PathBuf> {
    rollout_of(&Self::sessions_dir().ok()?, external_session_id, 4)
  }

  /// Zero until a response reports usage: the transcript records usage, so
  /// none yet means none used.
  fn context_size(&self, transcript: &Path) -> ContextSize {
    let Ok(text) = read_lossy(transcript, 0, None) else {
      return ContextSize::tokens(0);
    };
    ContextSize::tokens(
      text
        .lines()
        .rev()
        .take(50)
        .find_map(usage_of_line)
        .unwrap_or_default(),
    )
  }

  fn context_before(&self, transcript: &Path, offset: u64) -> ContextSize {
    ContextSize::tokens(
      read_lossy(transcript, 0, Some(offset))
        .map(|text| {
          text
            .lines()
            .filter_map(usage_of_line)
            .next_back()
            .unwrap_or(0)
        })
        .unwrap_or_default(),
    )
  }

  fn context_peak(&self, transcript: &Path, start: u64, end: Option<u64>) -> ContextSize {
    ContextSize::tokens(
      read_lossy(transcript, start, end)
        .map(|text| text.lines().filter_map(usage_of_line).max().unwrap_or(0))
        .unwrap_or_default(),
    )
  }

  /// Codex writes a prompt only when it takes it up, and writes user messages
  /// of its own (the AGENTS.md text, the environment context), so the prompt
  /// is matched by its text. One waiting behind the current turn is unseen
  /// until then.
  fn prompt_state(&self, transcript: &Path, offset: u64, prompt: &str) -> PromptState {
    let started = entries(transcript, offset).iter().any(|entry| {
      is_message_from(entry, "user")
        && text_of(entry.pointer("/payload/content").unwrap_or(&Value::Null)).contains(prompt)
    });
    if started {
      PromptState::Started
    } else {
      PromptState::Unseen
    }
  }

  fn latest_assistant_text(&self, transcript: &Path) -> Option<String> {
    let mut last = None;
    for entry in entries(transcript, 0) {
      if !is_message_from(&entry, "assistant") {
        continue;
      }
      if let Some(content) = entry.pointer("/payload/content").and_then(Value::as_array) {
        for block in content {
          if block.get("type").and_then(Value::as_str) == Some("output_text")
            && let Some(text) = block.get("text").and_then(Value::as_str)
            && !text.trim().is_empty()
          {
            last = Some(text.to_owned());
          }
        }
      }
    }
    last
  }

  /// What the agent said or did: its messages and its tool calls, not what
  /// the tools returned or what was said to it.
  fn output_mentions(&self, transcript: &Path, text: &str) -> bool {
    entries(transcript, 0)
      .iter()
      .any(|entry| is_agents_own(entry) && entry.to_string().contains(text))
  }
}

impl Codex {
  /// Where Codex keeps every rollout: `$CODEX_HOME/sessions`, or
  /// `~/.codex/sessions`.
  fn sessions_dir() -> Result<PathBuf> {
    let home = match env::var_os("CODEX_HOME") {
      Some(codex_home) => PathBuf::from(codex_home),
      None => PathBuf::from(env::var_os("HOME").context("HOME is not set")?).join(".codex"),
    };
    Ok(home.join("sessions"))
  }
}

/// The rollout named after the session, at most `depth` directories below
/// `dir`. A directory that cannot be read holds no rollout yet.
fn rollout_of(dir: &Path, external_session_id: &str, depth: usize) -> Option<PathBuf> {
  let suffix = format!("-{external_session_id}.jsonl");
  rollouts_below(dir, depth)
    .into_iter()
    .find(|path| file_name_of(path).ends_with(&suffix))
}

/// The id of the newest rollout at most `depth` directories below `dir`,
/// written at or after `since` by a session whose working directory is
/// `run_dir`.
fn newest_rollout_in(
  dir: &Path,
  run_dir: &Path,
  since: SystemTime,
  depth: usize,
) -> Option<String> {
  let mut rollouts: Vec<_> = rollouts_below(dir, depth)
    .into_iter()
    .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
    .filter(|(modified, _)| *modified >= since)
    .collect();
  rollouts.sort_by(|(left, _), (right, _)| right.cmp(left));
  rollouts
    .into_iter()
    .find_map(|(_, path)| session_of_rollout_in(&path, run_dir))
}

/// Every rollout at most `depth` directories below `dir`, in directory order.
fn rollouts_below(dir: &Path, depth: usize) -> Vec<PathBuf> {
  let Ok(entries) = fs::read_dir(dir) else {
    return Vec::new();
  };
  let mut rollouts = Vec::new();
  for path in entries
    .filter_map(std::result::Result::ok)
    .map(|entry| entry.path())
  {
    if path.is_dir() {
      if depth > 0 {
        rollouts.extend(rollouts_below(&path, depth - 1));
      }
    } else if file_name_of(&path).starts_with("rollout-") && file_name_of(&path).ends_with(".jsonl")
    {
      rollouts.push(path);
    }
  }
  rollouts
}

fn file_name_of(path: &Path) -> &str {
  path
    .file_name()
    .and_then(|name| name.to_str())
    .unwrap_or_default()
}

/// The session id a rollout's opening `session_meta` records, when that
/// session's working directory is `run_dir`.
fn session_of_rollout_in(rollout: &Path, run_dir: &Path) -> Option<String> {
  let mut first_line = String::new();
  std::io::BufReader::new(fs::File::open(rollout).ok()?)
    .read_line(&mut first_line)
    .ok()?;
  let meta: Value = serde_json::from_str(&first_line).ok()?;
  if meta.get("type").and_then(Value::as_str) != Some("session_meta") {
    return None;
  }
  let cwd = meta.pointer("/payload/cwd").and_then(Value::as_str)?;
  let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
  if cwd != run_dir {
    return None;
  }
  meta
    .pointer("/payload/id")
    .and_then(Value::as_str)
    .map(str::to_owned)
}

fn is_message_from(entry: &Value, role: &str) -> bool {
  entry.get("type").and_then(Value::as_str) == Some("response_item")
    && entry.pointer("/payload/type").and_then(Value::as_str) == Some("message")
    && entry.pointer("/payload/role").and_then(Value::as_str) == Some(role)
}

fn is_agents_own(entry: &Value) -> bool {
  entry.get("type").and_then(Value::as_str) == Some("response_item")
    && (entry.pointer("/payload/role").and_then(Value::as_str) == Some("assistant")
      || matches!(
        entry.pointer("/payload/type").and_then(Value::as_str),
        Some("function_call" | "custom_tool_call")
      ))
}

/// The context the model was handed for one response: the input tokens of
/// its `token_usage_record`. The cached count is part of that input, and the
/// thread totals on the same entry sum every response so far.
fn usage_of_line(line: &str) -> Option<u64> {
  let entry: Value = serde_json::from_str(line).ok()?;
  if entry.get("type").and_then(Value::as_str) != Some("token_usage_record") {
    return None;
  }
  entry
    .pointer("/payload/usage/input_tokens")
    .and_then(Value::as_u64)
}

#[cfg(test)]
mod tests;
