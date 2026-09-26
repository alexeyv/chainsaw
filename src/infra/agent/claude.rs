//! Claude Code: transcripts are JSONL under `~/.claude/projects/<munged cwd>/`,
//! one entry per turn, with the model's usage on every assistant entry.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{Value, json};

use super::{Agent, PromptState, read_lossy};
use crate::infra::session_runtime::SessionKind;

pub struct Claude;

impl Agent for Claude {
  fn name(&self) -> &'static str {
    "claude"
  }

  /// Today's flags; the commentator keeps slash commands
  fn default_args(&self, kind: SessionKind) -> String {
    let slash = match kind {
      SessionKind::Implementer => " --disable-slash-commands",
      SessionKind::Commentator => "",
    };
    format!(
      "--model opus --effort high{slash} --strict-mcp-config --no-chrome --disallowedTools WebSearch,WebFetch,NotebookEdit,Task,Agent,AskUserQuestion,EnterPlanMode,ExitPlanMode,TaskOutput"
    )
  }

  fn compact_prompt(&self) -> &'static str {
    "/compact"
  }

  /// Under the run directory when Claude Code agrees about the working
  /// directory, otherwise wherever it was found under the projects directory.
  fn transcript(&self, canonical_run_dir: &Path, external_session_id: &str) -> Option<PathBuf> {
    let under_run_dir = Self::transcript_under(canonical_run_dir, external_session_id).ok()?;
    if under_run_dir.is_file() {
      return Some(under_run_dir);
    }
    let filename = under_run_dir.file_name()?.to_owned();
    fs::read_dir(under_run_dir.parent()?.parent()?)
      .ok()?
      .filter_map(std::result::Result::ok)
      .map(|entry| entry.path().join(&filename))
      .find(|path| path.is_file())
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

  fn prompt_state(&self, transcript: &Path, offset: u64, prompt: &str) -> PromptState {
    let mut queued = false;
    for entry in entries(transcript, offset) {
      if entry.get("type").and_then(Value::as_str) == Some("user")
        && text_of(entry.pointer("/message/content").unwrap_or(&Value::Null)).contains(prompt)
      {
        return PromptState::Started;
      }
      if entry.get("type").and_then(Value::as_str) == Some("queue-operation")
        && entry.get("operation").and_then(Value::as_str) == Some("enqueue")
        && entry
          .get("content")
          .and_then(Value::as_str)
          .is_some_and(|content| content.contains(prompt))
      {
        queued = true;
      }
    }
    if queued {
      PromptState::Queued
    } else {
      PromptState::Unseen
    }
  }

  fn latest_assistant_text(&self, transcript: &Path) -> Option<String> {
    let mut last = None;
    for entry in entries(transcript, 0) {
      if entry.get("type").and_then(Value::as_str) != Some("assistant") {
        continue;
      }
      if let Some(content) = entry.pointer("/message/content").and_then(Value::as_array) {
        for block in content {
          if block.get("type").and_then(Value::as_str) == Some("text")
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

  fn output_mentions(&self, transcript: &Path, text: &str) -> bool {
    entries(transcript, 0).iter().any(|entry| {
      entry.get("type").and_then(Value::as_str) == Some("assistant")
        && entry.to_string().contains(text)
    })
  }
}

/// What Claude Code writes, for a runtime that stands in for it.
impl Claude {
  /// The entry Claude Code writes when it takes up a prompt.
  pub fn prompt_entry(text: &str) -> Value {
    json!({"type": "user", "message": {"content": text}})
  }

  /// The entry Claude Code writes when it queues a prompt sent while busy.
  pub fn queued_prompt_entry(text: &str) -> Value {
    json!({
      "type": "queue-operation",
      "operation": "enqueue",
      "content": text,
      "timestamp": Utc::now().to_rfc3339(),
    })
  }

  /// The entry Claude Code writes when it replies with text.
  pub fn reply_entry(text: &str) -> Value {
    json!({
      "type": "assistant",
      "message": {"content": [{"type": "text", "text": text}]},
    })
  }
}

/// Where Claude Code writes.
impl Claude {
  /// Where Claude Code keeps transcripts of sessions started in `run_dir`.
  /// The supervisor database lives here too: the lead runs inside Claude
  /// Code, so a run's state sits beside the logs it is derived from.
  pub fn transcripts_dir(canonical_run_dir: &Path) -> Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(
      PathBuf::from(home)
        .join(".claude")
        .join("projects")
        .join(transcripts_dir_name(canonical_run_dir)),
    )
  }

  /// The transcript of a session started in `run_dir`, under that
  /// directory's transcripts, whether or not it exists yet.
  pub fn transcript_under(canonical_run_dir: &Path, external_session_id: &str) -> Result<PathBuf> {
    Ok(Self::transcripts_dir(canonical_run_dir)?.join(format!("{external_session_id}.jsonl")))
  }
}

/// Claude Code names a session's transcripts directory after its cwd, replacing
/// both separators and dots with dashes: `/Users/alex/src/ui.wt/run` becomes
/// `-Users-alex-src-ui-wt-run`, and `/x/.bare` becomes `-x--bare`. Keeping the
/// dots put the database beside no transcript at all, and the commentator's
/// start message named a directory holding nothing (run of 2026-08-28).
fn transcripts_dir_name(canonical_run_dir: &Path) -> String {
  canonical_run_dir.to_string_lossy().replace(['/', '.'], "-")
}

fn usage_of_line(line: &str) -> Option<u64> {
  let entry: Value = serde_json::from_str(line).ok()?;
  if entry
    .get("isSidechain")
    .and_then(Value::as_bool)
    .unwrap_or(false)
  {
    return None;
  }
  let kind = entry.get("type")?.as_str()?;
  if kind != "assistant" && kind != "tool_result" {
    return None;
  }
  let usage = entry
    .pointer("/message/usage")
    .or_else(|| entry.get("usage"))?;
  Some(
    token_field(usage, "input_tokens")
      + token_field(usage, "cache_read_input_tokens")
      + token_field(usage, "cache_creation_input_tokens"),
  )
}

fn token_field(usage: &Value, key: &str) -> u64 {
  usage.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn entries(path: &Path, offset: u64) -> Vec<Value> {
  let Ok(text) = read_lossy(path, offset, None) else {
    return Vec::new();
  };
  text
    .lines()
    .filter_map(|line| serde_json::from_str(line).ok())
    .collect()
}

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
