//! Human-tuned settings, read from `chainsaw.json` in the run directory.
//!
//! These are inputs to a run, not state of it, so they live in a file the
//! human edits rather than in the supervisor database, which is disposable.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::agent::AgentSpec;
use crate::domain::Role;

pub const FILE_NAME: &str = "chainsaw.json";
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
  /// Reads `chainsaw.json` from `run_dir`. A missing file means defaults; a
  /// present file must be a JSON object whose known keys hold the documented types.
  pub fn load(run_dir: &Path) -> Result<Self> {
    let path = run_dir.join(FILE_NAME);
    match fs::read_to_string(&path) {
      Ok(text) => {
        Self::parse(&text).with_context(|| format!("invalid settings in {}", path.display()))
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
      Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
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
}

fn parse_agents(settings: &mut Settings, value: &Value) -> Result<()> {
  let Some(object) = value.as_object() else {
    bail!("setting \"agents\" must be a JSON object");
  };
  for (key, value) in object {
    let target = match key.as_str() {
      "lead" => &mut settings.lead,
      "implementer" => &mut settings.implementer,
      "commentator" => &mut settings.commentator,
      other => bail!("unknown agent role {other:?}; expected lead, implementer, or commentator"),
    };
    *target = AgentSpec::parse(value).with_context(|| format!("setting \"agents.{key}\""))?;
  }
  Ok(())
}

#[cfg(test)]
mod tests;
