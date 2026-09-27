//! User-editable settings, layered from optional TOML files: the global
//! `~/.config/chainsaw/chainsaw.toml` (or whatever `CHAINSAW_CONFIG` names),
//! then `chainsaw.toml` and `chainsaw.local.toml` in the run directory, each
//! laid over the previous key by key, except that a file naming a role's agent
//! also drops the args an earlier file gave that role. Loaded at the beginning
//! of each coordinator process.
//! Can be overridden with CLI args a la `--set prompt-timeout-seconds=20`

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use strum::IntoEnumIterator;
use toml::{Table, Value};

use super::agent;
use crate::domain::{AgentKind, SessionKind};

pub const FILE_NAME: &str = "chainsaw.toml";
pub const LOCAL_FILE_NAME: &str = "chainsaw.local.toml";
pub const GLOBAL_FILE_ENV: &str = "CHAINSAW_CONFIG";
const LEGACY_FILE_NAME: &str = "chainsaw.json";

/// The global settings file: `$CHAINSAW_CONFIG`, none when that is set but
/// empty, or `$XDG_CONFIG_HOME/chainsaw/chainsaw.toml` when it is unset,
/// falling back to `~/.config/chainsaw/chainsaw.toml`
fn global_file() -> Result<Option<PathBuf>> {
  if let Some(path) = env::var_os(GLOBAL_FILE_ENV) {
    return Ok((!path.is_empty()).then(|| PathBuf::from(path)));
  }
  let config_home = match env::var_os("XDG_CONFIG_HOME") {
    Some(dir) if !dir.is_empty() => PathBuf::from(dir),
    _ => PathBuf::from(env::var_os("HOME").context("HOME is not set")?).join(".config"),
  };
  Ok(Some(config_home.join("chainsaw").join(FILE_NAME)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
  prompt_timeout: Duration,
  implementer: LaunchSettings,
  commentator: LaunchSettings,
}

/// What a session of one kind starts with: its agent and that agent's flags
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchSettings {
  agent: AgentKind,
  args: Vec<String>,
}

impl Settings {
  /// Reads the global file, then `chainsaw.toml` and `chainsaw.local.toml`
  /// from `run_dir`, lays each over the previous (see `lay_over`), then lays
  /// every `--set` over the result as one more layer. A missing file lays
  /// nothing over; a leftover `chainsaw.json` is an error
  pub fn load(run_dir: &Path, sets: &[String]) -> Result<Self> {
    Self::load_from(global_file()?.as_deref(), run_dir, sets)
  }

  /// `load` with the global file at `global_file`, or without one
  fn load_from(global_file: Option<&Path>, run_dir: &Path, sets: &[String]) -> Result<Self> {
    if run_dir.join(LEGACY_FILE_NAME).exists() {
      bail!(
        "{LEGACY_FILE_NAME} is no longer used; transfer its settings to {FILE_NAME} and delete"
      );
    }
    let table = global_file
      .map(|path| (path.to_path_buf(), path.display().to_string()))
      .into_iter()
      .chain([
        (run_dir.join(FILE_NAME), FILE_NAME.to_owned()),
        (run_dir.join(LOCAL_FILE_NAME), LOCAL_FILE_NAME.to_owned()),
      ])
      .try_fold(Table::new(), |table, (path, name)| {
        read_layer(&path, &name).map(|layer| lay_over(table, layer))
      })?;
    let table = lay_over(table, sets_layer(sets)?);
    Self::from_table(&table)
      .map_err(|error| anyhow!("invalid settings: {}", error.to_string().trim_end()))
  }

  fn from_table(table: &Table) -> Result<Self> {
    Self::build(table.clone().try_into()?)
  }

  fn build(file: File) -> Result<Self> {
    let launch = |kind: SessionKind, role: Option<RoleSettings>| -> Result<LaunchSettings> {
      let role = role.unwrap_or_default();
      let agent = match role.agent {
        None => AgentKind::Claude,
        Some(name) => AgentKind::try_from(name.as_str()).map_err(|error| {
          anyhow!(
            "{error}, expected {}\nin `{}.agent`",
            accepted_agents(),
            kind.label()
          )
        })?,
      };
      let args = role
        .args
        .unwrap_or_else(|| agent::implementing(agent).default_args(kind));
      let args = shell_words::split(&args)
        .map_err(|error| anyhow!("{error}\nin `{}.args`", kind.label()))?;
      Ok(LaunchSettings { agent, args })
    };
    Ok(Self {
      prompt_timeout: Duration::from_secs(file.prompt_timeout_seconds.unwrap_or(15)),
      implementer: launch(SessionKind::Implementer, file.implementer)?,
      commentator: launch(SessionKind::Commentator, file.commentator)?,
    })
  }

  /// How long a sent prompt gets to reach the transcript before it is resent
  pub fn prompt_timeout(&self) -> Duration {
    self.prompt_timeout
  }

  /// The agent a session of this kind launches with
  pub fn launch_agent(&self, kind: SessionKind) -> AgentKind {
    self.launch(kind).agent
  }

  /// The agent flags a session of this kind launches with
  pub fn launch_args(&self, kind: SessionKind) -> &[String] {
    &self.launch(kind).args
  }

  fn launch(&self, kind: SessionKind) -> &LaunchSettings {
    match kind {
      SessionKind::Implementer => &self.implementer,
      SessionKind::Commentator => &self.commentator,
    }
  }
}

/// The accepted agent names, worded the way serde words an expected field
fn accepted_agents() -> String {
  let names: Vec<String> = AgentKind::iter()
    .map(|agent| format!("`{agent}`"))
    .collect();
  match names.as_slice() {
    [only] => only.clone(),
    names => format!("one of {}", names.join(", ")),
  }
}

/// The shape of chainsaw.toml. Every key is optional; serde rejects unknown
/// keys and wrong types
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct File {
  prompt_timeout_seconds: Option<u64>,
  implementer: Option<RoleSettings>,
  commentator: Option<RoleSettings>,
}

/// `agent` names the coding agent the role runs, Claude when left out.
/// `args` is the whole flag list, split like a shell would (quotes group a
/// value with spaces) and passed to the agent verbatim
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RoleSettings {
  agent: Option<String>,
  args: Option<String>,
}

/// Parses and validates one settings file, called `name` in errors. A
/// missing file is an empty table
fn read_layer(path: &Path, name: &str) -> Result<Table> {
  let invalid = |error: anyhow::Error| {
    anyhow!(
      "invalid settings in {name}: {}",
      error.to_string().trim_end()
    )
  };
  let table = match fs::read_to_string(path) {
    Ok(text) => text
      .parse::<Table>()
      .map_err(|error| invalid(error.into()))?,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Table::new()),
    Err(error) => bail!("cannot read {name}: {error}"),
  };
  Settings::from_table(&table).map_err(invalid)?;
  Ok(table)
}

/// Builds one settings layer from every `--set`, as one file would be: the
/// order of the sets does not matter, and two sets naming the same key are an
/// error. Each error names the set it is about
fn sets_layer(sets: &[String]) -> Result<Table> {
  sets
    .iter()
    .enumerate()
    .try_fold(Table::new(), |layer, (index, set)| {
      set_into(layer, set, &sets[..index])
        .map_err(|error| anyhow!("invalid --set {set}: {}", error.to_string().trim_end()))
    })
}

/// Merges one validated `KEY=VALUE` into `layer`, unless an `earlier` set
/// named the same key. The value is a TOML literal when it parses as one
/// (`20`, `"x"`), otherwise a string
fn set_into(layer: Table, set: &str, earlier: &[String]) -> Result<Table> {
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
  let table: Table = format!("{key} = {literal}").parse()?;
  Settings::from_table(&table)?;
  Ok(merge(layer, table))
}

/// Lays one settings layer over `table` key by key, except that a role naming
/// its `agent` without its `args` also drops the `args` earlier layers gave
/// that role: the role then gets the named agent's defaults instead of flags
/// written for another agent
fn lay_over(mut table: Table, layer: Table) -> Table {
  for (key, value) in &layer {
    if let Value::Table(role) = value
      && role.contains_key("agent")
      && !role.contains_key("args")
      && let Some(Value::Table(current)) = table.get_mut(key)
    {
      current.remove("args");
    }
  }
  merge(table, layer)
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

#[cfg(test)]
mod tests;
