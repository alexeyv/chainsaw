//! Human-tuned settings, read from `chainsaw.json`.
//!
//! These are inputs to a run, not state of it, so they live in files the human
//! edits rather than in the supervisor database, which is disposable. A global
//! file supplies defaults; the run directory overlays named keys and roles.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::agent::{self, AgentSpec};
use crate::domain::Role;

pub const FILE_NAME: &str = "chainsaw.json";
pub const GLOBAL_CONFIG_ENV: &str = "CHAINSAW_CONFIG";
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
  /// Global file, then `chainsaw.json` in `run_dir`. Either file may be absent.
  /// A present file must be a JSON object whose known keys hold the documented types.
  pub fn load(run_dir: &Path) -> Result<Self> {
    load_from(run_dir, global_config_path().as_deref())
  }

  pub fn parse(text: &str) -> Result<Self> {
    let mut settings = Self::default();
    settings.apply(text)?;
    Ok(settings)
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

  fn apply(&mut self, text: &str) -> Result<()> {
    let value: Value = serde_json::from_str(text)?;
    let Some(object) = value.as_object() else {
      bail!("expected a JSON object at the top level");
    };
    for (key, value) in object {
      match key.as_str() {
        "prompt-landing-seconds" => {
          self.prompt_landing_seconds = value
            .as_i64()
            .with_context(|| format!("setting {key:?} must be an integer, got {value}"))?;
        }
        "agents" => parse_agents(self, value)?,
        other => bail!("unknown setting {other:?}"),
      }
    }
    Ok(())
  }

  fn apply_file(&mut self, path: &Path) -> Result<()> {
    match fs::read_to_string(path) {
      Ok(text) => self
        .apply(&text)
        .map_err(|error| anyhow!("invalid settings in {}: {error}", path.display())),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
      Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
  }
}

/// `CHAINSAW_CONFIG` names a file. Unset: `~/.config/chainsaw/chainsaw.json`.
/// Empty: no global file.
pub fn global_config_path() -> Option<PathBuf> {
  global_config_path_from(
    env::var_os(GLOBAL_CONFIG_ENV),
    agent::home_dir().ok().as_deref(),
  )
}

fn global_config_path_from(config_env: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
  match config_env {
    Some(value) if value.is_empty() => None,
    Some(value) => Some(PathBuf::from(value)),
    None => Some(home?.join(".config/chainsaw").join(FILE_NAME)),
  }
}

fn load_from(run_dir: &Path, global: Option<&Path>) -> Result<Settings> {
  let mut settings = Settings::default();
  if let Some(path) = global {
    settings.apply_file(path)?;
  }
  settings.apply_file(&run_dir.join(FILE_NAME))?;
  Ok(settings)
}

fn parse_agents(settings: &mut Settings, value: &Value) -> Result<()> {
  let Some(object) = value.as_object() else {
    bail!("setting \"agents\" must be a JSON object");
  };
  for (key, value) in object {
    let spec = AgentSpec::parse(value).with_context(|| format!("setting \"agents.{key}\""))?;
    match key.as_str() {
      "lead" => settings.lead = spec,
      "implementer" => settings.implementer = spec,
      "commentator" => settings.commentator = spec,
      other => bail!("unknown agent role {other:?}"),
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests;
