//! The run as every command sees it: its checkout, its database, the runtime
//! its sessions live in, and its settings. `Run` carries no behavior of its
//! own yet; commands are functions over it.

use std::path::Path;

use anyhow::Result;

use crate::infra::git::Repo;
use crate::infra::session_runtime::{self, SessionRuntime};
use crate::infra::settings::Settings;
use crate::infra::store::Store;

pub struct Run {
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
      store,
      repo,
      runtime,
      settings,
    })
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
