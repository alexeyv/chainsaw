//! Cursor Agent CLI: transcripts are JSONL under
//! `~/.cursor/projects/<project>/agent-transcripts/<session id>/<session id>.jsonl`,
//! one entry per message. Entries are only `{"role":"user",…}`,
//! `{"role":"assistant",…}` with `text` and `tool_use` blocks, and
//! `{"type":"turn_ended",…}`: no token usage and no tool results, so the
//! transcript cannot say how much context the session holds or what git
//! printed (Cursor 2026.08.11). The file is not append-only either: a new
//! turn drops the trailing `turn_ended` line and writes it again at the end.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{entries, read_lossy, text_of};
use crate::domain::{Agent, ContextSize, PromptEcho, PromptState, SessionKind};

/// The prompt a new session is launched with.
pub const LAUNCH_PROMPT: &str = "Reply only with the word ready, then wait for the task.";

pub struct Cursor;

impl Agent for Cursor {
  /// The CLI's own name: `cursor` on PATH opens the IDE.
  fn program(&self) -> &'static str {
    "cursor-agent"
  }

  /// An unattended session edits, builds, tests and commits without asking:
  /// `--trust` skips the workspace trust prompt and `--force` lets every
  /// command run without approval. A `git commit` succeeds under Cursor's
  /// default sandbox with `--force` (verified on 2026.08.11), so the sandbox
  /// stays on. The model stays whatever Cursor's own configuration says. The
  /// trailing `.` is the session's first prompt: Cursor writes its
  /// transcript, and so has a session id to report, only once a prompt lands.
  /// Cursor writes a session's transcript, and so its id, only once a prompt
  /// has been answered; this first one asks for nothing more. A bare `.`
  /// once drew a chooser, which then swallowed the task sent into it.
  fn default_args(&self, _kind: SessionKind) -> String {
    format!("--trust --force {}", shell_words::quote(LAUNCH_PROMPT))
  }

  /// Cursor's own compaction command. The daemon sends it only past a context
  /// threshold, and Cursor's context is never known, so it cannot fire;
  /// Cursor summarizes a long conversation by itself.
  fn compact_prompt(&self) -> &'static str {
    "/summarize"
  }

  /// Cursor names its own sessions: `--resume` takes only an existing chat.
  fn session_id_args(&self, _id: &str) -> Option<Vec<String>> {
    None
  }

  /// Cursor writes a session's transcript only once its first prompt lands,
  /// which is why the default args end in one.
  fn session_started_since(&self, canonical_run_dir: &Path, since: SystemTime) -> Option<String> {
    newest_session_in(&Self::projects_dir().ok()?, canonical_run_dir, since)
  }

  /// Under the run directory's project when Cursor agrees about the working
  /// directory, otherwise wherever it was found under the projects directory.
  fn transcript(&self, canonical_run_dir: &Path, external_session_id: &str) -> Option<PathBuf> {
    transcript_in(
      &Self::projects_dir().ok()?,
      canonical_run_dir,
      external_session_id,
    )
  }

  /// Cursor's transcript records no usage, so the context is unknown rather
  /// than zero.
  fn context_size(&self, _transcript: &Path) -> ContextSize {
    ContextSize::UNKNOWN
  }

  fn context_before(&self, _transcript: &Path, _offset: u64) -> ContextSize {
    ContextSize::UNKNOWN
  }

  fn context_peak(&self, _transcript: &Path, _start: u64, _end: Option<u64>) -> ContextSize {
    ContextSize::UNKNOWN
  }

  /// Cursor writes a prompt only when it takes it up, wrapped in a timestamp
  /// and a `user_query` element, so the prompt is matched by its text. One
  /// waiting behind the current turn is unseen until then. The turn that takes
  /// it drops the `turn_ended` line the transcript ended with, so an offset
  /// taken before the send may now fall inside the prompt's own line.
  fn prompt_state(&self, transcript: &Path, offset: u64, prompt: &str) -> PromptState {
    let started = entries_from_line_holding(transcript, offset)
      .iter()
      .any(|entry| is_from(entry, "user") && content_text(entry).contains(prompt));
    if started {
      PromptState::Started
    } else {
      PromptState::Unseen
    }
  }

  /// Cursor writes the prompt together with its first reply, which in a real
  /// run came 39 seconds after the prompt was sent, 16 seconds after the
  /// commit it asked for had already landed.
  fn prompt_echo(&self) -> PromptEcho {
    PromptEcho::WithReply
  }

  fn latest_assistant_text(&self, transcript: &Path) -> Option<String> {
    let mut last = None;
    for entry in entries(transcript, 0) {
      if !is_from(&entry, "assistant") {
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

  /// What the agent said or did: its text and its tool calls, which share an
  /// assistant entry. Tool results are not in the transcript at all.
  fn output_mentions(&self, transcript: &Path, text: &str) -> bool {
    entries(transcript, 0)
      .iter()
      .any(|entry| is_from(entry, "assistant") && entry.to_string().contains(text))
  }

  /// The transcript keeps no tool output, so git's commit line never reaches
  /// it: HEAD is the only candidate.
  fn commit_candidates(&self, _transcript: &Path, _offset: u64, head: &str) -> Vec<String> {
    vec![head.to_owned()]
  }
}

impl Cursor {
  /// Where Cursor keeps every project's transcripts: `~/.cursor/projects`.
  /// `CURSOR_CONFIG_DIR` moves only `cli-config.json` and chats; transcripts
  /// stay under the home directory (verified on 2026.08.11).
  fn projects_dir() -> Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".cursor").join("projects"))
  }
}

/// The session's transcript under `projects`: under the run directory's own
/// project first, else wherever a file named after the session sits within
/// three directories of `projects`.
fn transcript_in(
  projects: &Path,
  canonical_run_dir: &Path,
  external_session_id: &str,
) -> Option<PathBuf> {
  let filename = format!("{external_session_id}.jsonl");
  let under_project = projects
    .join(project_name(canonical_run_dir))
    .join("agent-transcripts")
    .join(external_session_id)
    .join(&filename);
  if under_project.is_file() {
    return Some(under_project);
  }
  transcript_named(projects, &filename, 3)
}

/// Cursor names a project directory after its working directory with every
/// character outside `[A-Za-z0-9]` turned into a dash, runs of dashes
/// collapsed and the ends trimmed: `/a/b.c_d` becomes `a-b-c-d` and `/x/.bare`
/// becomes `x-bare` (Cursor's own `workspace-paths.js`, 2026.08.11).
fn project_name(canonical_run_dir: &Path) -> String {
  let mut name = String::new();
  for character in canonical_run_dir.to_string_lossy().chars() {
    if character.is_ascii_alphanumeric() {
      name.push(character);
    } else if !name.ends_with('-') {
      name.push('-');
    }
  }
  name.trim_matches('-').to_owned()
}

/// The id of the session under the run directory's own project whose
/// transcript was written last, at or after `since`. Only that project: the
/// runtime starts the agent in the run directory, so Cursor files the session
/// there, and a session of some other project running at the same moment must
/// not be taken for it.
fn newest_session_in(
  projects: &Path,
  canonical_run_dir: &Path,
  since: SystemTime,
) -> Option<String> {
  let transcripts = projects
    .join(project_name(canonical_run_dir))
    .join("agent-transcripts");
  files_below(&transcripts, 1)
    .into_iter()
    .filter(|path| is_transcript(path))
    .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
    .filter(|(modified, _)| *modified >= since)
    .max_by_key(|(modified, _)| *modified)
    .and_then(|(_, path)| Some(path.file_stem()?.to_str()?.to_owned()))
}

/// A session's transcript is the `.jsonl` named after the directory it sits in.
fn is_transcript(path: &Path) -> bool {
  path
    .extension()
    .is_some_and(|extension| extension == "jsonl")
    && path.file_stem() == path.parent().and_then(Path::file_name)
}

/// The file called `filename` at most `depth` directories below `dir`.
fn transcript_named(dir: &Path, filename: &str, depth: usize) -> Option<PathBuf> {
  files_below(dir, depth)
    .into_iter()
    .find(|path| path.file_name().is_some_and(|name| name == filename))
}

/// Every file at most `depth` directories below `dir`, in directory order. A
/// directory that cannot be read holds nothing yet.
fn files_below(dir: &Path, depth: usize) -> Vec<PathBuf> {
  let Ok(entries) = fs::read_dir(dir) else {
    return Vec::new();
  };
  let mut files = Vec::new();
  for path in entries
    .filter_map(std::result::Result::ok)
    .map(|entry| entry.path())
  {
    if path.is_dir() {
      if depth > 0 {
        files.extend(files_below(&path, depth - 1));
      }
    } else {
      files.push(path);
    }
  }
  files
}

/// Every entry from the line holding byte `offset` on. When the transcript
/// grew by appending, that line starts at `offset`; when a new turn dropped
/// the `turn_ended` line before it, the content after `offset` has moved back
/// by that line and the entry that now holds `offset` is the first new one.
fn entries_from_line_holding(transcript: &Path, offset: u64) -> Vec<Value> {
  let Ok(text) = read_lossy(transcript, 0, None) else {
    return Vec::new();
  };
  let offset = usize::try_from(offset)
    .unwrap_or(usize::MAX)
    .min(text.len());
  let start = text.as_bytes()[..offset]
    .iter()
    .rposition(|&byte| byte == b'\n')
    .map_or(0, |newline| newline + 1);
  text[start..]
    .lines()
    .filter_map(|line| serde_json::from_str(line).ok())
    .collect()
}

fn is_from(entry: &Value, role: &str) -> bool {
  entry.get("role").and_then(Value::as_str) == Some(role)
}

fn content_text(entry: &Value) -> String {
  text_of(entry.pointer("/message/content").unwrap_or(&Value::Null))
}

#[cfg(test)]
mod tests;
