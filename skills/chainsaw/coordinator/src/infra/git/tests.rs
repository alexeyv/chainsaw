use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

use super::{Commit, Repo};

static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

/// A fresh repository in a temporary directory, removed on drop.
struct Fixture {
  dir: PathBuf,
}

impl Fixture {
  fn new() -> Self {
    let suffix = NEXT_REPO.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("chainsaw-git-{}-{suffix}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    Self { dir }
  }

  fn repo(&self) -> Repo {
    Repo::new(&self.dir)
  }

  /// Writes `file` with `content` and commits it with `message`, returning
  /// the full id of the commit.
  fn commit(&self, file: &str, content: &str, message: &str) -> String {
    fs::write(self.dir.join(file), content).unwrap();
    git(&self.dir, &["add", file]);
    git(&self.dir, &["commit", "-q", "-m", message]);
    git(&self.dir, &["rev-parse", "HEAD"])
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.dir);
  }
}

fn git(dir: &Path, args: &[&str]) -> String {
  let output = Command::new("git")
    .arg("-C")
    .arg(dir)
    .args([
      "-c",
      "user.name=test",
      "-c",
      "user.email=test@example.com",
      "-c",
      "commit.gpgsign=false",
    ])
    .args(args)
    .output()
    .unwrap();
  assert!(
    output.status.success(),
    "git {args:?} failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

mod canonical_commit {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let sha = fixture.commit("a.txt", "a", "first");
    let repo = fixture.repo();

    assert_eq!(repo.canonical_commit(&sha[..7])?, Some(sha));
    assert_eq!(repo.canonical_commit("HEAD")?, None);
    assert_eq!(repo.canonical_commit("abc")?, None);
    assert_eq!(repo.canonical_commit("0123456789abcdef")?, None);
    Ok(())
  }
}

mod new_commit_among {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let older = fixture.commit("a.txt", "a", "first");
    let base = fixture.commit("b.txt", "b", "second");
    let third = fixture.commit("c.txt", "c", "third");
    let newest = fixture.commit("d.txt", "d", "fourth");
    let repo = fixture.repo();

    assert_eq!(repo.head()?, newest);
    assert!(repo.has_commit(&newest)?);
    assert!(repo.is_ancestor(&base, &base)?);
    assert_eq!(repo.files_changed(&base, &newest)?, ["c.txt", "d.txt"]);

    let skipped = [
      older[..7].to_owned(),
      "0123456789abcdef".to_owned(),
      base[..7].to_owned(),
    ];
    assert_eq!(repo.new_commit_among(&skipped, &base)?, None);

    let mut candidates = vec![third[..7].to_owned(), newest[..7].to_owned()];
    candidates.extend(skipped);
    assert_eq!(
      repo.new_commit_among(&candidates, &base)?,
      Some(newest[..7].to_owned())
    );
    Ok(())
  }
}

mod head_advanced_cleanly_from {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let base = fixture.commit("a.txt", "a", "first");
    assert!(!repo.head_advanced_cleanly_from(&base)?);

    let next = fixture.commit("b.txt", "b", "second");
    assert!(repo.head_advanced_cleanly_from(&base)?);

    fs::write(fixture.dir.join("a.txt"), "changed")?;
    fs::write(fixture.dir.join("new.txt"), "new")?;
    assert_eq!(repo.status()?, "M a.txt\n?? new.txt");
    assert!(!repo.head_advanced_cleanly_from(&base)?);

    git(&fixture.dir, &["reset", "-q", "--hard", &base]);
    fs::remove_file(fixture.dir.join("new.txt"))?;
    assert_eq!(repo.status()?, "");
    assert!(!repo.head_advanced_cleanly_from(&next)?);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_repository_has_no_commits() {
    let fixture = Fixture::new();

    assert_eq!(
      fixture.repo().head().unwrap_err().to_string(),
      "HEAD names no commit; an empty repository needs a first commit"
    );
  }
}

mod commit {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    fixture.commit("a.txt", "a\nb\n", "first");
    let message = "second\n\nCo-authored-by: someone";
    let sha = fixture.commit("a.txt", "a\nc\nd\n", message);
    let repo = fixture.repo();

    assert_eq!(
      repo.shortstat(&sha)?,
      "1 file changed, 2 insertions(+), 1 deletion(-)"
    );
    assert!(repo.files_changed(&sha, "HEAD")?.is_empty());
    assert_eq!(
      repo.commit(&sha[..7])?,
      Some(Commit {
        sha,
        message: message.to_owned()
      })
    );
    assert_eq!(repo.commit("0123456789abcdef")?, None);
    Ok(())
  }
}
