//! Human-tuned settings, read from `chainsaw.toml` in the run directory.
//!
//! These are inputs to a run, not state of it, so they live in a file the
//! human edits rather than in the supervisor database, which is disposable.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use toml::Table;

use crate::agent::AgentSpec;
use crate::domain::Role;

pub const FILE_NAME: &str = "chainsaw.toml";
/// The settings file before it became TOML; still present means a stale setup.
const RETIRED_FILE_NAME: &str = "chainsaw.json";
pub const DEFAULT_PROMPT_LANDING_SECONDS: i64 = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
  prompt_landing_seconds: i64,
  lead: AgentSpec,
  implementer: AgentSpec,
  commentator: AgentSpec,
}

impl Default for Settings {
  fn default() -> Self {
    Self {
      prompt_landing_seconds: DEFAULT_PROMPT_LANDING_SECONDS,
      lead: AgentSpec::claude_opus(),
      implementer: AgentSpec::claude_opus(),
      commentator: AgentSpec::claude_opus(),
    }
  }
}

impl Settings {
  /// Reads the text of `chainsaw.toml` from `run_dir`, empty when the file is
  /// absent. The text must parse as settings. A leftover `chainsaw.json` is an
  /// error, so settings are never silently ignored. A run reads this once;
  /// see `Store::open`.
  pub fn read_file(run_dir: &Path) -> Result<String> {
    let retired = run_dir.join(RETIRED_FILE_NAME);
    if retired.exists() {
      bail!(
        "{} is no longer read; move its settings to {} as TOML and delete it",
        retired.display(),
        run_dir.join(FILE_NAME).display()
      );
    }
    let path = run_dir.join(FILE_NAME);
    match fs::read_to_string(&path) {
      Ok(text) => {
        Self::parse(&text).with_context(|| format!("invalid settings in {}", path.display()))?;
        Ok(text)
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
      Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
  }

  pub fn parse(text: &str) -> Result<Self> {
    let table: Table = text.parse()?;
    reject_unknown(&table, &["prompt-landing-seconds", "agents"], "setting", "")?;
    let prompt_landing_seconds = match table.get("prompt-landing-seconds") {
      Some(value) => value.as_integer().with_context(|| {
        format!("setting \"prompt-landing-seconds\" must be an integer, got {value}")
      })?,
      None => DEFAULT_PROMPT_LANDING_SECONDS,
    };
    let agents = match table.get("agents") {
      Some(value) => value
        .as_table()
        .context("setting \"agents\" must be a table")?,
      None => &Table::new(),
    };
    reject_unknown(
      agents,
      &["lead", "implementer", "commentator"],
      "agent role",
      "; expected lead, implementer, or commentator",
    )?;
    Ok(Self {
      prompt_landing_seconds,
      lead: agent_spec(agents, "lead")?,
      implementer: agent_spec(agents, "implementer")?,
      commentator: agent_spec(agents, "commentator")?,
    })
  }

  pub fn prompt_landing_seconds(&self) -> i64 {
    self.prompt_landing_seconds
  }

  pub fn agent(&self, role: Role) -> &AgentSpec {
    match role {
      Role::Lead => &self.lead,
      Role::Implementer => &self.implementer,
      Role::Commentator => &self.commentator,
    }
  }
}

fn reject_unknown(table: &Table, known: &[&str], what: &str, hint: &str) -> Result<()> {
  match table.keys().find(|key| !known.contains(&key.as_str())) {
    Some(key) => bail!("unknown {what} {key:?}{hint}"),
    None => Ok(()),
  }
}

/// The role's spec from `[agents.<role>]`, or Claude Code on Opus when absent.
fn agent_spec(agents: &Table, role: &str) -> Result<AgentSpec> {
  match agents.get(role) {
    Some(value) => AgentSpec::parse(value).with_context(|| format!("setting \"agents.{role}\"")),
    None => Ok(AgentSpec::claude_opus()),
  }
}

#[cfg(test)]
mod tests;
