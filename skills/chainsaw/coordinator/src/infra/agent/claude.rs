//! Claude Code: transcripts are JSONL under `~/.claude/projects/<munged cwd>/`,
//! one entry per turn, with the model's usage on every assistant entry.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{
  TranscriptFormat, commits_printed, entries, read_lossy, start_with_prompt, text_of,
  transcript_file,
};
use crate::domain::{
  Agent, AgentKind, ContextSize, Launched, PromptState, SessionKind, SessionRuntime, StartSession,
  Transcript,
};

pub struct Claude;

impl Agent for Claude {
  fn kind(&self) -> AgentKind {
    AgentKind::Claude
  }

  fn program(&self) -> &'static str {
    "claude"
  }

  /// The prompt goes in on the command line, and the agent writes it to its
  /// transcript before its first reply.
  fn start(
    &self,
    runtime: &dyn SessionRuntime,
    session: StartSession<'_>,
    prompt: &str,
  ) -> Result<Launched> {
    start_with_prompt(self, runtime, session, prompt)
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

  fn session_id_args(&self, id: &str) -> Option<Vec<String>> {
    Some(vec!["--session-id".to_owned(), id.to_owned()])
  }

  fn session_started_since(&self, canonical_run_dir: &Path, since: SystemTime) -> Option<String> {
    newest_transcript_in(&Self::transcripts_dir(canonical_run_dir).ok()?, since)
  }

  /// Under the run directory's project folder when Claude Code named it as
  /// chainsaw does, otherwise whichever project folder holds a file named
  /// after the session. The lead's id comes from the human, so a session
  /// resumed from elsewhere can have a stale copy in another folder.
  fn transcript(&self, canonical_run_dir: &Path, external_session_id: &str) -> Option<PathBuf> {
    transcript_of(
      &Self::projects_dir().ok()?,
      canonical_run_dir,
      external_session_id,
    )
  }

  fn transcript_at(&self, path: &Path) -> Box<dyn Transcript> {
    transcript_file(&Claude, path)
  }
}

impl TranscriptFormat for Claude {
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

  fn context_peak(
    &self,
    transcript: &Path,
    from_offset: u64,
    to_offset: Option<u64>,
  ) -> ContextSize {
    ContextSize::tokens(
      read_lossy(transcript, from_offset, to_offset)
        .map(|text| text.lines().filter_map(usage_of_line).max().unwrap_or(0))
        .unwrap_or_default(),
    )
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

  /// The transcript shows what git printed, so its commit lines are read.
  fn commit_candidates(&self, transcript: &Path, offset: u64, _head: &str) -> Vec<String> {
    commits_printed(transcript, offset)
  }
}

/// Where Claude Code writes.
impl Claude {
  /// Where Claude Code keeps one transcripts folder per working directory.
  fn projects_dir() -> Result<PathBuf> {
    let home = env::home_dir().context("cannot find the home directory")?;
    Ok(home.join(".claude").join("projects"))
  }

  /// Where Claude Code keeps transcripts of sessions started in `run_dir`.
  fn transcripts_dir(canonical_run_dir: &Path) -> Result<PathBuf> {
    Ok(Self::projects_dir()?.join(transcripts_dir_name(canonical_run_dir)))
  }
}

/// The transcript of `external_session_id` in the run directory's project
/// folder under `projects`, else in any project folder directly under it.
fn transcript_of(
  projects: &Path,
  canonical_run_dir: &Path,
  external_session_id: &str,
) -> Option<PathBuf> {
  let filename = format!("{external_session_id}.jsonl");
  let under_run_dir = projects
    .join(transcripts_dir_name(canonical_run_dir))
    .join(&filename);
  if under_run_dir.is_file() {
    return Some(under_run_dir);
  }
  fs::read_dir(projects)
    .ok()?
    .filter_map(std::result::Result::ok)
    .map(|entry| entry.path().join(&filename))
    .find(|path| path.is_file())
}

/// The session whose transcript directly under `dir` was written last, at or
/// after `since`. A directory that cannot be read holds no transcript yet.
fn newest_transcript_in(dir: &Path, since: SystemTime) -> Option<String> {
  fs::read_dir(dir)
    .ok()?
    .filter_map(std::result::Result::ok)
    .map(|entry| entry.path())
    .filter(|path| {
      path
        .extension()
        .is_some_and(|extension| extension == "jsonl")
    })
    .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
    .filter(|(modified, _)| *modified >= since)
    .max_by_key(|(modified, _)| *modified)
    .and_then(|(_, path)| Some(path.file_stem()?.to_str()?.to_owned()))
}

/// Claude Code names a session's transcripts directory after its cwd, replacing
/// every character but ASCII letters and digits with a dash:
/// `/Users/alex/src/ui.wt/run` becomes `-Users-alex-src-ui-wt-run`, `/x/.bare`
/// becomes `-x--bare`, and `C:\x\y_z` becomes `C--x-y-z`. Dashing only
/// separators and dots missed run directories holding `_` or spaces.
fn transcripts_dir_name(canonical_run_dir: &Path) -> String {
  canonical_run_dir
    .to_string_lossy()
    .chars()
    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
    .collect()
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

#[cfg(test)]
mod tests;
