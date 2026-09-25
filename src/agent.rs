//! Which interactive CLI a role runs, and how to launch and find it.
//!
//! The supervisor talks to Herdr; Herdr starts `claude`, `cursor-agent`, or
//! `codex`. Each role in `chainsaw.toml` picks a CLI and a model. Transcripts
//! stay where that CLI writes them.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml::Value;

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
    if args.iter().any(|argument| argument.trim().is_empty()) {
      bail!("agent extra arg cannot be blank");
    }
    Ok(Self { cli, model, args })
  }

  pub fn parse(value: &Value) -> Result<Self> {
    let Some(table) = value.as_table() else {
      bail!("agent spec must be a table");
    };
    if let Some(other) = table
      .keys()
      .find(|key| !["cli", "model", "args"].contains(&key.as_str()))
    {
      bail!("unknown agent setting {other:?}");
    }
    let cli = match table.get("cli") {
      Some(value) => AgentCli::parse(value.as_str().context("agent cli must be a string")?)?,
      None => bail!("agent spec needs a cli"),
    };
    let model = match table.get("model") {
      Some(value) => Some(
        value
          .as_str()
          .context("agent model must be a string")?
          .to_owned(),
      ),
      None if cli == AgentCli::Claude => Some("opus".to_owned()),
      None => None,
    };
    let args = match table.get("args") {
      Some(value) => value
        .as_array()
        .context("agent args must be an array of strings")?
        .iter()
        .map(|item| {
          item
            .as_str()
            .map(str::to_owned)
            .context("agent args must be an array of strings")
        })
        .collect::<Result<_>>()?,
      None => Vec::new(),
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

  /// Flags passed to the CLI after `herdr agent start … --`: the CLI's
  /// defaults, then the role's extra `args`.
  pub fn launch_flags(&self, kind: SessionKind) -> Vec<String> {
    let defaults = match self.cli {
      AgentCli::Claude => claude_flags(self.model.as_deref().unwrap_or("opus"), kind),
      AgentCli::Cursor => cursor_flags(self.model.as_deref()),
      AgentCli::Codex => model_flag(self.model.as_deref()),
    };
    defaults
      .into_iter()
      .chain(self.args.iter().map(String::as_str))
      .map(str::to_owned)
      .collect()
  }
}

fn claude_flags(model: &str, kind: SessionKind) -> Vec<&str> {
  let slash_commands: &[&str] = match kind {
    SessionKind::Implementer => &["--disable-slash-commands"],
    SessionKind::Commentator => &[],
  };
  ["--model", model, "--effort", "high"]
    .into_iter()
    .chain(slash_commands.iter().copied())
    .chain([
      "--strict-mcp-config",
      "--no-chrome",
      "--disallowedTools",
      CLAUDE_DISALLOWED_TOOLS,
    ])
    .collect()
}

fn cursor_flags(model: Option<&str>) -> Vec<&str> {
  ["--trust", "--force"]
    .into_iter()
    .chain(model_flag(model))
    .collect()
}

fn model_flag(model: Option<&str>) -> Vec<&str> {
  model
    .map(|model| vec!["--model", model])
    .unwrap_or_default()
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

/// The session's transcript wherever its CLI wrote it, or None while it does
/// not exist yet. A missing CLI home is "not yet"; any other failure to look
/// is an error.
pub fn find_session_transcript(
  cli: AgentCli,
  canonical_run_dir: &Path,
  session_id: &str,
) -> Result<Option<PathBuf>> {
  let path = expected_transcript(cli, canonical_run_dir, session_id)?;
  if path.is_file() {
    return Ok(Some(path));
  }
  match cli {
    AgentCli::Claude => {
      let projects = claude_home()?.join("projects");
      find_named_jsonl(&projects, &format!("{session_id}.jsonl"), 2)
    }
    AgentCli::Cursor => {
      let projects = cursor_home()?.join("projects");
      find_named_jsonl(&projects, &format!("{session_id}.jsonl"), 4)
    }
    AgentCli::Codex => {
      let sessions = codex_home()?.join("sessions");
      find_jsonl_containing(&sessions, session_id, 4)
    }
  }
  .with_context(|| format!("cannot look for the {cli} transcript of session {session_id}"))
}

fn find_named_jsonl(root: &Path, filename: &str, max_depth: usize) -> Result<Option<PathBuf>> {
  walk(root, max_depth, &mut |path| {
    path.file_name().and_then(|name| name.to_str()) == Some(filename)
  })
}

fn find_jsonl_containing(root: &Path, needle: &str, max_depth: usize) -> Result<Option<PathBuf>> {
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
) -> Result<Option<PathBuf>> {
  match fs::metadata(root) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error).with_context(|| format!("cannot read {}", root.display())),
    Ok(_) => {}
  }
  walk_from(root, 0, max_depth, predicate)
}

fn walk_from(
  dir: &Path,
  depth: usize,
  max_depth: usize,
  predicate: &mut impl FnMut(&Path) -> bool,
) -> Result<Option<PathBuf>> {
  if depth > max_depth {
    return Ok(None);
  }
  let entries = fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?;
  let mut dirs = Vec::new();
  for entry in entries {
    let path = entry
      .with_context(|| format!("cannot read {}", dir.display()))?
      .path();
    if path.is_dir() {
      dirs.push(path);
    } else if predicate(&path) {
      return Ok(Some(path));
    }
  }
  for dir in dirs {
    if let Some(found) = walk_from(&dir, depth + 1, max_depth, predicate)? {
      return Ok(Some(found));
    }
  }
  Ok(None)
}

#[cfg(test)]
mod tests;
