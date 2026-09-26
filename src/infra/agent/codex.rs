//! OpenAI Codex: rollouts are JSONL under `$CODEX_HOME/sessions/<year>/<month>/<day>/`,
//! named `rollout-<started at>-<session id>.jsonl`, one entry per event, with
//! the model's usage on a `token_usage_record` entry after every response.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use super::{Agent, PromptState, entries, read_lossy, text_of};
use crate::infra::session_runtime::SessionKind;

pub struct Codex;

impl Agent for Codex {
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

  /// Codex shards rollouts by date rather than by working directory, so the
  /// run directory plays no part: the rollout is whichever file under the
  /// sessions directory is named after the session.
  fn transcript(&self, _canonical_run_dir: &Path, external_session_id: &str) -> Option<PathBuf> {
    rollout_of(&Self::sessions_dir().ok()?, external_session_id, 4)
  }

  fn context_size(&self, transcript: &Path) -> u64 {
    let Ok(text) = read_lossy(transcript, 0, None) else {
      return 0;
    };
    text
      .lines()
      .rev()
      .take(50)
      .find_map(usage_of_line)
      .unwrap_or_default()
  }

  fn context_before(&self, transcript: &Path, offset: u64) -> u64 {
    read_lossy(transcript, 0, Some(offset))
      .map(|text| {
        text
          .lines()
          .filter_map(usage_of_line)
          .next_back()
          .unwrap_or(0)
      })
      .unwrap_or_default()
  }

  fn context_peak(&self, transcript: &Path, start: u64, end: Option<u64>) -> u64 {
    read_lossy(transcript, start, end)
      .map(|text| text.lines().filter_map(usage_of_line).max().unwrap_or(0))
      .unwrap_or_default()
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
  let mut subdirectories = Vec::new();
  for entry in fs::read_dir(dir).ok()?.filter_map(std::result::Result::ok) {
    let path = entry.path();
    if path.is_dir() {
      subdirectories.push(path);
    } else if path
      .file_name()
      .and_then(|name| name.to_str())
      .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(&suffix))
    {
      return Some(path);
    }
  }
  if depth == 0 {
    return None;
  }
  subdirectories
    .into_iter()
    .find_map(|subdirectory| rollout_of(&subdirectory, external_session_id, depth - 1))
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
