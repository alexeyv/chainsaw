//! The run as every command sees it: its directory, its checkout, the runtime
//! its sessions live in, and its settings. `Run` carries no behavior of its
//! own yet; commands are functions over it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::infra::agent::Claude;
use crate::infra::git::Repo;
use crate::infra::session_runtime::{self, SessionRuntime};
use crate::infra::settings::Settings;
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
    let runtime = session_runtime::from_environment(run_dir)?;
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
}
