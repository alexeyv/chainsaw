//! The run as every command sees it: its directory, its checkout, the runtime
//! its sessions live in, and its settings. `Run` carries no behavior of its
//! own yet; commands are functions over it.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::Transaction;

use chrono::{DateTime, Utc};

use crate::domain::{AgentKind, ContextSize, Role, Session, SessionRuntime};
use crate::infra::agent::{self, Claude};
use crate::infra::git::Repo;
use crate::infra::session_runtime::{HerdrSessionRuntime, OrcaSessionRuntime};
use crate::infra::settings::Settings;
use crate::persistence::session;
use crate::persistence::store::DATABASE_FILE_NAME;

pub struct Run {
  dir: PathBuf,
  transcripts_dir: PathBuf,
  prompt_lock_path: PathBuf,
  repo: Repo,
  runtime: Box<dyn SessionRuntime>,
  settings: Settings,
}

impl Run {
  /// Opens the run rooted at `run_dir`, with `overrides` applied over its
  /// settings files as `--set KEY=VALUE` pairs.
  pub fn open(run_dir: &Path, overrides: &[String]) -> Result<Self> {
    let runtime = runtime_from_environment(run_dir)?;
    let settings = Settings::load(run_dir, overrides)?;
    let dir = run_dir
      .canonicalize()
      .with_context(|| format!("cannot resolve run directory {}", run_dir.display()))?;
    let transcripts_dir = Claude::transcripts_dir(&dir)?;
    fs::create_dir_all(&transcripts_dir)?;
    let prompt_lock_path = PathBuf::from(format!(
      "{}.prompt-lock",
      transcripts_dir.join(DATABASE_FILE_NAME).display()
    ));
    Ok(Self {
      repo: Repo::new(&dir),
      dir,
      transcripts_dir,
      prompt_lock_path,
      runtime,
      settings,
    })
  }

  /// The run's clean-slate checkout, canonicalized.
  pub fn dir(&self) -> &Path {
    &self.dir
  }

  /// Where the run's session transcripts and durable supervisor state live,
  /// under `~/.claude/projects/`.
  pub fn transcripts_dir(&self) -> &Path {
    &self.transcripts_dir
  }

  /// The file every `prompt` command holds a lock on while it sends, so two
  /// prompts to the same run never interleave.
  pub fn prompt_lock_path(&self) -> &Path {
    &self.prompt_lock_path
  }

  pub fn repo(&self) -> &Repo {
    &self.repo
  }

  pub fn runtime(&self) -> &dyn SessionRuntime {
    self.runtime.as_ref()
  }

  pub fn settings(&self) -> &Settings {
    &self.settings
  }

  // Every session is built here, so every session drives itself through
  // this run's runtime and reads its transcript through its agent.

  pub fn sessions(&self, transaction: &Transaction<'_>) -> Result<Vec<Session<'_>>> {
    session::all(transaction, self.runtime(), agent::implementing)
  }

  pub fn session(&self, transaction: &Transaction<'_>, id: i64) -> Result<Option<Session<'_>>> {
    session::get(transaction, self.runtime(), agent::implementing, id)
  }

  /// The newest incarnation of the session called `name`, live or not.
  pub fn session_named(
    &self,
    transaction: &Transaction<'_>,
    name: &str,
  ) -> Result<Option<Session<'_>>> {
    session::latest_named(transaction, self.runtime(), agent::implementing, name)
  }

  /// Registers a session its runtime has just started.
  pub fn register_session(
    &self,
    transaction: &Transaction<'_>,
    name: &str,
    role: Role,
    agent: AgentKind,
    external_session_id: &str,
    launched_head: Option<&str>,
  ) -> Result<Session<'_>> {
    session::create(
      transaction,
      self.runtime(),
      agent::implementing,
      name,
      role,
      agent,
      external_session_id,
      launched_head,
    )
  }

  pub fn record_session_transcript(
    &self,
    transaction: &Transaction<'_>,
    id: i64,
    path: &Path,
  ) -> Result<Session<'_>> {
    session::record_transcript(transaction, self.runtime(), agent::implementing, id, path)
  }

  pub fn record_session_reading(
    &self,
    transaction: &Transaction<'_>,
    id: i64,
    context: ContextSize,
    grew: bool,
    at: DateTime<Utc>,
  ) -> Result<Session<'_>> {
    session::record_reading(
      transaction,
      self.runtime(),
      agent::implementing,
      id,
      context,
      grew,
      at,
    )
  }

  pub fn record_session_kick(&self, transaction: &Transaction<'_>, id: i64) -> Result<Session<'_>> {
    session::record_kick(transaction, self.runtime(), agent::implementing, id)
  }

  pub fn record_session_over_limit(
    &self,
    transaction: &Transaction<'_>,
    id: i64,
  ) -> Result<Session<'_>> {
    session::record_over_limit(transaction, self.runtime(), agent::implementing, id)
  }
}

/// The runtime the supervisor was started under: Herdr inside a Herdr pane,
/// Orca inside an Orca terminal. A Herdr pane opened from an Orca terminal
/// sees both and is a Herdr pane. Outside both, Herdr, which then refuses to
/// start a session. The tests put a `herdr` of their own on PATH.
fn runtime_from_environment(run_dir: &Path) -> Result<Box<dyn SessionRuntime>> {
  Ok(
    match runtime_named_by(
      env::var_os("HERDR_WORKSPACE_ID").is_some(),
      env::var("ORCA_TERMINAL_HANDLE").ok(),
    ) {
      Some(terminal) => Box::new(OrcaSessionRuntime::from_environment(run_dir, terminal)?),
      None => Box::new(HerdrSessionRuntime::from_environment()),
    },
  )
}

/// The Orca terminal to drive sessions from, or None for Herdr.
fn runtime_named_by(inside_herdr: bool, orca_terminal: Option<String>) -> Option<String> {
  orca_terminal.filter(|_| !inside_herdr)
}

#[cfg(test)]
mod tests;
