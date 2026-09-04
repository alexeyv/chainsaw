//! Human-tuned settings, read from `chainsaw.json` in the run directory.
//!
//! These are inputs to a run, not state of it, so they live in a file the
//! human edits rather than in the supervisor database, which is disposable.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

pub const FILE_NAME: &str = "chainsaw.json";
pub const DEFAULT_PROMPT_LANDING_SECONDS: i64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
  prompt_landing_seconds: i64,
}

impl Default for Settings {
  fn default() -> Self {
    Self {
      prompt_landing_seconds: DEFAULT_PROMPT_LANDING_SECONDS,
    }
  }
}

impl Settings {
  /// Reads `chainsaw.json` from `run_dir`. A missing file means defaults; a
  /// present file must be a JSON object whose known keys hold integers.
  pub fn load(run_dir: &Path) -> Result<Self> {
    let path = run_dir.join(FILE_NAME);
    match fs::read_to_string(&path) {
      Ok(text) => Self::parse(&text)
        .map_err(|error| anyhow!("invalid settings in {}: {error}", path.display())),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
      Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
  }

  pub fn parse(text: &str) -> Result<Self> {
    let value: Value = serde_json::from_str(text)?;
    let Some(object) = value.as_object() else {
      bail!("expected a JSON object at the top level");
    };
    let mut settings = Self::default();
    for (key, value) in object {
      let target = match key.as_str() {
        "prompt-landing-seconds" => &mut settings.prompt_landing_seconds,
        other => bail!("unknown setting {other:?}"),
      };
      *target = value
        .as_i64()
        .with_context(|| format!("setting {key:?} must be an integer, got {value}"))?;
    }
    Ok(settings)
  }

  pub fn prompt_landing_seconds(&self) -> i64 {
    self.prompt_landing_seconds
  }
}

#[cfg(test)]
mod tests;
