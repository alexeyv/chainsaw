//! User-editable settings, living in an optional chainsaw.toml file.
//! Loaded at the beginning of each coordinator process.
//! Can be overridden with CLI args a la `--set prompt-landing-seconds=20`

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use toml::{Table, Value};

use crate::session_runtime::SessionKind;

pub const FILE_NAME: &str = "chainsaw.toml";
const LEGACY_FILE_NAME: &str = "chainsaw.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
  prompt_landing: Duration,
  implementer_args: Vec<String>,
  commentator_args: Vec<String>,
}

impl Settings {
  /// Reads `chainsaw.toml` from `run_dir` and applies each `--set` over it.
  /// A missing file means defaults; a leftover `chainsaw.json` is an error
  pub fn load(run_dir: &Path, sets: &[String]) -> Result<Self> {
    if run_dir.join(LEGACY_FILE_NAME).exists() {
      bail!(
        "{LEGACY_FILE_NAME} is no longer used; transfer its settings to {FILE_NAME} and delete"
      );
    }
    let invalid = |error: anyhow::Error| {
      anyhow!(
        "invalid settings in {FILE_NAME}: {}",
        error.to_string().trim_end()
      )
    };
    let table = match fs::read_to_string(run_dir.join(FILE_NAME)) {
      Ok(text) => text
        .parse::<Table>()
        .map_err(|error| invalid(error.into()))?,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Table::new(),
      Err(error) => bail!("cannot read {FILE_NAME}: {error}"),
    };
    let base = Self::from_table(&table).map_err(invalid)?;
    let (_, settings) =
      sets
        .iter()
        .enumerate()
        .try_fold((table, base), |(table, _), (index, set)| {
          set_over(table, set, &sets[..index])
            .and_then(|table| Self::from_table(&table).map(|settings| (table, settings)))
            .map_err(|error| anyhow!("invalid --set {set}: {}", error.to_string().trim_end()))
        })?;
    Ok(settings)
  }

  fn from_table(table: &Table) -> Result<Self> {
    Self::build(table.clone().try_into()?)
  }

  fn build(file: File) -> Result<Self> {
    let launch = |kind, role: Option<Role>| {
      let args = role
        .unwrap_or_default()
        .args
        .unwrap_or_else(|| default_args(kind));
      shell_words::split(&args).map_err(|error| anyhow!("{error}\nin `{}.args`", kind.label()))
    };
    Ok(Self {
      prompt_landing: Duration::from_secs(file.prompt_landing_seconds.unwrap_or(15)),
      implementer_args: launch(SessionKind::Implementer, file.implementer)?,
      commentator_args: launch(SessionKind::Commentator, file.commentator)?,
    })
  }

  /// How long a sent prompt gets to reach the transcript before it is resent
  pub fn prompt_landing(&self) -> Duration {
    self.prompt_landing
  }

  /// The Claude flags a session of this kind launches with
  pub fn launch_args(&self, kind: SessionKind) -> &[String] {
    match kind {
      SessionKind::Implementer => &self.implementer_args,
      SessionKind::Commentator => &self.commentator_args,
    }
  }
}

/// The shape of chainsaw.toml. Every key is optional; serde rejects unknown
/// keys and wrong types
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct File {
  prompt_landing_seconds: Option<u64>,
  implementer: Option<Role>,
  commentator: Option<Role>,
}

/// `args` is the whole flag list, split like a shell would (quotes group a
/// value with spaces) and passed to Claude verbatim
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Role {
  args: Option<String>,
}

/// Lays one `KEY=VALUE` over `table`, unless an `earlier` set named the same
/// key. The value is a TOML literal when it parses as one (`20`, `"x"`),
/// otherwise a string
fn set_over(table: Table, set: &str, earlier: &[String]) -> Result<Table> {
  let Some((key, raw)) = set.split_once('=') else {
    bail!("expected KEY=VALUE");
  };
  if earlier
    .iter()
    .any(|set| set.split_once('=').is_some_and(|(seen, _)| seen == key))
  {
    bail!("{key} was already set by an earlier --set");
  }
  let literal = match format!("v = {raw}").parse::<Table>() {
    Ok(_) => raw.to_owned(),
    Err(_) => Value::String(raw.to_owned()).to_string(),
  };
  Ok(merge(table, format!("{key} = {literal}").parse()?))
}

/// Lays `source` over `target`, descending into tables both sides have
fn merge(target: Table, source: Table) -> Table {
  source.into_iter().fold(target, |mut target, (key, value)| {
    let value = match (target.remove(&key), value) {
      (Some(Value::Table(current)), Value::Table(value)) => Value::Table(merge(current, value)),
      (_, value) => value,
    };
    target.insert(key, value);
    target
  })
}

/// Today's flags; the commentator keeps slash commands
fn default_args(kind: SessionKind) -> String {
  let slash = match kind {
    SessionKind::Implementer => " --disable-slash-commands",
    SessionKind::Commentator => "",
  };
  format!(
    "--model opus --effort high{slash} --strict-mcp-config --no-chrome --disallowedTools WebSearch,WebFetch,NotebookEdit,Task,Agent,AskUserQuestion,EnterPlanMode,ExitPlanMode,TaskOutput"
  )
}

#[cfg(test)]
mod tests;
