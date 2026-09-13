//! Which interactive CLI a role runs, and how to launch and find it.
//!
//! The supervisor talks to Herdr; Herdr starts `claude`, `cursor-agent`, or
//! `codex`. Each role in `chainsaw.json` picks a CLI and a model. Transcripts
//! stay where that CLI writes them.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::session_runtime::SessionKind;
use crate::store::project_directory_name;

const CLAUDE_DISALLOWED_TOOLS: &str = "WebSearch,WebFetch,NotebookEdit,Task,Agent,AskUserQuestion,EnterPlanMode,ExitPlanMode,TaskOutput";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentCli {
  Claude,
  Cursor,
  Codex,
}

impl AgentCli {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Claude => "claude",
      Self::Cursor => "cursor",
      Self::Codex => "codex",
    }
  }

  pub fn herdr_kind(self) -> &'static str {
    self.as_str()
  }

  pub fn parse(value: &str) -> Result<Self> {
    match value {
      "claude" | "claude-code" => Ok(Self::Claude),
      "cursor" | "cursor-cli" | "cursor-agent" => Ok(Self::Cursor),
      "codex" | "codex-cli" => Ok(Self::Codex),
      value => bail!("unknown agent cli {value:?}; expected claude, cursor, or codex"),
    }
  }
}

impl std::fmt::Display for AgentCli {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter.write_str(self.as_str())
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSpec {
  cli: AgentCli,
  model: Option<String>,
  args: Vec<String>,
}

impl AgentSpec {
  pub fn claude_opus() -> Self {
    Self {
      cli: AgentCli::Claude,
      model: Some("opus".to_owned()),
      args: Vec::new(),
    }
  }

  pub fn new(cli: AgentCli, model: Option<String>, args: Vec<String>) -> Result<Self> {
    if let Some(model) = &model
      && model.trim().is_empty()
    {
      bail!("agent model cannot be blank");
    }
    for argument in &args {
      if argument.trim().is_empty() {
        bail!("agent extra arg cannot be blank");
      }
    }
    Ok(Self { cli, model, args })
  }

  pub fn parse(value: &Value) -> Result<Self> {
    let Some(object) = value.as_object() else {
      bail!("agent spec must be a JSON object");
    };
    let mut cli = None;
    let mut model = None;
    let mut args = Vec::new();
    for (key, value) in object {
      match key.as_str() {
        "cli" => {
          let text = value.as_str().context("agent cli must be a string")?;
          cli = Some(AgentCli::parse(text)?);
        }
        "model" => {
          let text = value.as_str().context("agent model must be a string")?;
          model = Some(text.to_owned());
        }
        "args" => {
          let Some(items) = value.as_array() else {
            bail!("agent args must be an array of strings");
          };
          for item in items {
            let text = item
              .as_str()
              .context("agent args must be an array of strings")?;
            args.push(text.to_owned());
          }
        }
        other => bail!("unknown agent setting {other:?}"),
      }
    }
    let cli = cli.ok_or_else(|| anyhow::anyhow!("agent spec needs a cli"))?;
    let model = match (cli, model) {
      (AgentCli::Claude, None) => Some("opus".to_owned()),
      (_, model) => model,
    };
    Self::new(cli, model, args)
  }

  pub fn cli(&self) -> AgentCli {
    self.cli
  }

  pub fn model(&self) -> Option<&str> {
    self.model.as_deref()
  }

  pub fn args(&self) -> &[String] {
    &self.args
  }

  /// Flags passed to the CLI after `herdr agent start … --`.
  pub fn launch_flags(&self, kind: SessionKind) -> Vec<String> {
    let mut flags = match self.cli {
      AgentCli::Claude => claude_flags(self.model.as_deref().unwrap_or("opus"), kind),
      AgentCli::Cursor => cursor_flags(self.model.as_deref()),
      AgentCli::Codex => self
        .model
        .as_ref()
        .map(|model| vec!["--model".to_owned(), model.clone()])
        .unwrap_or_default(),
    };
    flags.extend(self.args.iter().cloned());
    flags
  }
}

fn claude_flags(model: &str, kind: SessionKind) -> Vec<String> {
  let mut flags = vec![
    "--model".to_owned(),
    model.to_owned(),
    "--effort".to_owned(),
    "high".to_owned(),
  ];
  if matches!(kind, SessionKind::Implementer) {
    flags.push("--disable-slash-commands".to_owned());
  }
  flags.extend([
    "--strict-mcp-config".to_owned(),
    "--no-chrome".to_owned(),
    "--disallowedTools".to_owned(),
    CLAUDE_DISALLOWED_TOOLS.to_owned(),
  ]);
  flags
}

fn cursor_flags(model: Option<&str>) -> Vec<String> {
  let mut flags = vec!["--trust".to_owned(), "--force".to_owned()];
  if let Some(model) = model {
    flags.push("--model".to_owned());
    flags.push(model.to_owned());
  }
  flags
}

pub fn home_dir() -> Result<PathBuf> {
  env::var_os("HOME")
    .map(PathBuf::from)
    .ok_or_else(|| anyhow::anyhow!("HOME is not set"))
}

pub fn claude_home() -> Result<PathBuf> {
  if let Some(dir) = env::var_os("CLAUDE_CONFIG_DIR") {
    return Ok(PathBuf::from(dir));
  }
  Ok(home_dir()?.join(".claude"))
}

pub fn cursor_home() -> Result<PathBuf> {
  if let Some(dir) = env::var_os("CURSOR_CONFIG_DIR") {
    return Ok(PathBuf::from(dir));
  }
  Ok(home_dir()?.join(".cursor"))
}

pub fn codex_home() -> Result<PathBuf> {
  if let Some(dir) = env::var_os("CODEX_HOME") {
    return Ok(PathBuf::from(dir));
  }
  Ok(home_dir()?.join(".codex"))
}

/// Cursor names a project by the cwd with slashes turned into dashes and the
/// leading slash dropped: `/Users/a/src/app` becomes `Users-a-src-app`.
pub fn cursor_project_directory_name(canonical_run_dir: &Path) -> String {
  canonical_run_dir
    .to_string_lossy()
    .trim_start_matches('/')
    .replace('/', "-")
}

pub fn expected_transcript(
  cli: AgentCli,
  canonical_run_dir: &Path,
  session_id: &str,
) -> Result<PathBuf> {
  Ok(match cli {
    AgentCli::Claude => claude_home()?
      .join("projects")
      .join(project_directory_name(canonical_run_dir))
      .join(format!("{session_id}.jsonl")),
    AgentCli::Cursor => cursor_home()?
      .join("projects")
      .join(cursor_project_directory_name(canonical_run_dir))
      .join("agent-transcripts")
      .join(session_id)
      .join(format!("{session_id}.jsonl")),
    AgentCli::Codex => {
      // Codex shards by date; the expected path is unknown until the file
      // exists. Callers should use `find_session_transcript`.
      codex_home()?
        .join("sessions")
        .join(format!("{session_id}.jsonl"))
    }
  })
}

pub fn find_session_transcript(
  cli: AgentCli,
  canonical_run_dir: &Path,
  session_id: &str,
) -> Option<PathBuf> {
  if let Ok(path) = expected_transcript(cli, canonical_run_dir, session_id)
    && path.is_file()
  {
    return Some(path);
  }
  match cli {
    AgentCli::Claude => {
      let projects = claude_home().ok()?.join("projects");
      find_named_jsonl(&projects, &format!("{session_id}.jsonl"), 2)
    }
    AgentCli::Cursor => {
      let projects = cursor_home().ok()?.join("projects");
      let filename = format!("{session_id}.jsonl");
      find_named_jsonl(&projects, &filename, 4)
    }
    AgentCli::Codex => {
      let sessions = codex_home().ok()?.join("sessions");
      find_jsonl_containing(&sessions, session_id, 4)
    }
  }
}

fn find_named_jsonl(root: &Path, filename: &str, max_depth: usize) -> Option<PathBuf> {
  walk(root, max_depth, &mut |path| {
    path.file_name().and_then(|name| name.to_str()) == Some(filename)
  })
}

fn find_jsonl_containing(root: &Path, needle: &str, max_depth: usize) -> Option<PathBuf> {
  walk(root, max_depth, &mut |path| {
    path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
      && path
        .file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(needle))
  })
}

fn walk(
  root: &Path,
  max_depth: usize,
  predicate: &mut impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
  walk_from(root, 0, max_depth, predicate)
}

fn walk_from(
  dir: &Path,
  depth: usize,
  max_depth: usize,
  predicate: &mut impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
  if depth > max_depth {
    return None;
  }
  let entries = fs::read_dir(dir).ok()?;
  let mut dirs = Vec::new();
  for entry in entries.filter_map(Result::ok) {
    let path = entry.path();
    if path.is_dir() {
      dirs.push(path);
    } else if predicate(&path) {
      return Some(path);
    }
  }
  for dir in dirs {
    if let Some(found) = walk_from(&dir, depth + 1, max_depth, predicate) {
      return Some(found);
    }
  }
  None
}

#[cfg(test)]
mod tests;
