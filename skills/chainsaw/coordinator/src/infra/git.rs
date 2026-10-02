//! The run's git repository as the supervisor reads it: where HEAD stands,
//! whether a commit exists and descends from another, and what one changed.
//! Every call runs the git CLI in the run directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result};

/// One commit as `git log` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
  /// The full 40-character id.
  pub sha: String,
  pub message: String,
}

#[derive(Clone, Debug)]
pub struct Repo {
  dir: PathBuf,
}

impl Repo {
  pub fn new(dir: &Path) -> Self {
    Self {
      dir: dir.to_path_buf(),
    }
  }

  /// The full id HEAD points at.
  pub fn head(&self) -> Result<String> {
    self.stdout(&["rev-parse", "HEAD"])
  }

  /// Whether `sha` names an object in the repository.
  pub fn has_commit(&self, sha: &str) -> Result<bool> {
    Ok(self.run(&["cat-file", "-e", sha])?.status.success())
  }

  /// Whether `ancestor` is `descendant` itself or one of its ancestors.
  pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
    Ok(
      self
        .run(&["merge-base", "--is-ancestor", ancestor, descendant])?
        .status
        .success(),
    )
  }

  /// The full id of the commit an abbreviated hex `sha` names, or None when
  /// `sha` is not hex of a plausible length or names no commit.
  pub fn canonical_commit(&self, sha: &str) -> Result<Option<String>> {
    if !(7..=40).contains(&sha.len())
      || !sha
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
      return Ok(None);
    }
    let revision = format!("{sha}^{{commit}}");
    let output = self.run(&["rev-parse", "--verify", &revision])?;
    if !output.status.success() {
      return Ok(None);
    }
    Ok(Some(text(&output)))
  }

  /// The newest of `candidates`, given oldest first, that is a commit other
  /// than `base_head` descending from it.
  pub fn new_commit_among(&self, candidates: &[String], base_head: &str) -> Result<Option<String>> {
    for sha in candidates.iter().rev() {
      if base_head.starts_with(sha.as_str()) {
        continue;
      }
      if !self.has_commit(sha)? {
        continue;
      }
      if !self.is_ancestor(base_head, sha)? {
        continue;
      }
      return Ok(Some(sha.clone()));
    }
    Ok(None)
  }

  /// Whether HEAD has moved on from `base_head` along its own line and the
  /// tree is clean: the state a task's commit leaves the repository in.
  pub fn head_advanced_cleanly_from(&self, base_head: &str) -> Result<bool> {
    let head = self.head()?;
    if head.is_empty() || head == base_head {
      return Ok(false);
    }
    if !self.is_ancestor(base_head, &head)? {
      return Ok(false);
    }
    self.is_clean()
  }

  /// The working tree's changes in porcelain form; empty when clean.
  pub fn status(&self) -> Result<String> {
    self.stdout(&["status", "--porcelain"])
  }

  pub fn is_clean(&self) -> Result<bool> {
    Ok(self.status()?.is_empty())
  }

  /// The commit `sha` names, or None when git has none by that name.
  pub fn commit(&self, sha: &str) -> Result<Option<Commit>> {
    let output = self.run(&["log", "-1", "--format=%H%n%B", sha])?;
    if !output.status.success() {
      return Ok(None);
    }
    let shown = String::from_utf8_lossy(&output.stdout);
    let (sha, message) = shown.split_once('\n').unwrap_or((shown.trim(), ""));
    Ok(Some(Commit {
      sha: sha.to_owned(),
      message: message.trim_end().to_owned(),
    }))
  }

  /// The `N files changed, N insertions(+), N deletions(-)` line of `sha`.
  pub fn shortstat(&self, sha: &str) -> Result<String> {
    self.stdout(&["show", "--shortstat", "--format=", sha])
  }

  /// The paths that differ between the two commits.
  pub fn files_changed(&self, from: &str, to: &str) -> Result<Vec<String>> {
    Ok(
      self
        .stdout(&["diff", "--name-only", &format!("{from}..{to}")])?
        .lines()
        .map(str::to_owned)
        .collect(),
    )
  }

  fn run(&self, args: &[&str]) -> Result<Output> {
    Command::new("git")
      .arg("-C")
      .arg(&self.dir)
      .args(args)
      .output()
      .context("failed to run git")
  }

  fn stdout(&self, args: &[&str]) -> Result<String> {
    Ok(text(&self.run(args)?))
  }
}

fn text(output: &Output) -> String {
  String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[cfg(test)]
mod tests;
