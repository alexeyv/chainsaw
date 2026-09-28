//! The run as every command sees it: its directory, its checkout, its
//! database, the runtime its sessions live in, and its settings. `Run` carries
//! no behavior of its own yet; commands are functions over it.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::infra::git::Repo;
use crate::infra::session_runtime::{self, SessionRuntime};
use crate::infra::settings::Settings;
use crate::infra::store::Store;

pub struct Run {
  dir: PathBuf,
  transcripts_dir: PathBuf,
  store: Store,
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
    let store = Store::open(run_dir)?;
    let repo = Repo::new(&store.run_dir);
    Ok(Self {
      dir: store.run_dir.clone(),
      transcripts_dir: store.transcripts_dir.clone(),
      store,
      repo,
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

  pub fn store(&self) -> &Store {
    &self.store
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
