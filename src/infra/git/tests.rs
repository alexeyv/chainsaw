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

  fn repo(&self) -> Repo<'_> {
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

mod head {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let sha = fixture.commit("a.txt", "a", "first");

    assert_eq!(fixture.repo().head()?, sha);
    Ok(())
  }
}

mod has_commit {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let sha = fixture.commit("a.txt", "a", "first");

    assert!(fixture.repo().has_commit(&sha)?);
    assert!(fixture.repo().has_commit(&sha[..7])?);
    Ok(())
  }

  #[test]
  fn should_be_false_when_the_sha_names_nothing() -> Result<()> {
    let fixture = Fixture::new();
    fixture.commit("a.txt", "a", "first");

    assert!(!fixture.repo().has_commit("0123456789abcdef")?);
    Ok(())
  }
}

mod is_ancestor {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let first = fixture.commit("a.txt", "a", "first");
    let second = fixture.commit("b.txt", "b", "second");

    assert!(fixture.repo().is_ancestor(&first, &second)?);
    assert!(fixture.repo().is_ancestor(&first, &first)?);
    Ok(())
  }

  #[test]
  fn should_be_false_when_the_descendant_comes_first() -> Result<()> {
    let fixture = Fixture::new();
    let first = fixture.commit("a.txt", "a", "first");
    let second = fixture.commit("b.txt", "b", "second");

    assert!(!fixture.repo().is_ancestor(&second, &first)?);
    Ok(())
  }
}

mod canonical_commit {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let sha = fixture.commit("a.txt", "a", "first");

    assert_eq!(fixture.repo().canonical_commit(&sha[..7])?, Some(sha));
    Ok(())
  }

  #[test]
  fn should_be_none_when_the_sha_is_not_hex_of_a_plausible_length() -> Result<()> {
    let fixture = Fixture::new();
    fixture.commit("a.txt", "a", "first");

    assert_eq!(fixture.repo().canonical_commit("HEAD")?, None);
    assert_eq!(fixture.repo().canonical_commit("abc")?, None);
    Ok(())
  }

  #[test]
  fn should_be_none_when_no_commit_has_the_sha() -> Result<()> {
    let fixture = Fixture::new();
    fixture.commit("a.txt", "a", "first");

    assert_eq!(fixture.repo().canonical_commit("0123456789abcdef")?, None);
    Ok(())
  }
}

mod new_commit_among {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let base = fixture.commit("a.txt", "a", "first");
    let second = fixture.commit("b.txt", "b", "second");
    let third = fixture.commit("c.txt", "c", "third");
    let candidates = vec![
      base[..7].to_owned(),
      second[..7].to_owned(),
      third[..7].to_owned(),
    ];

    let found = fixture.repo().new_commit_among(&candidates, &base)?;

    assert_eq!(found, Some(third[..7].to_owned()));
    Ok(())
  }

  #[test]
  fn should_skip_candidates_that_are_not_commits_descending_from_the_base() -> Result<()> {
    let fixture = Fixture::new();
    let older = fixture.commit("a.txt", "a", "first");
    let base = fixture.commit("b.txt", "b", "second");
    let newer = fixture.commit("c.txt", "c", "third");
    let candidates = vec![
      newer[..7].to_owned(),
      older[..7].to_owned(),
      "0123456789abcdef".to_owned(),
      base[..7].to_owned(),
    ];

    let found = fixture.repo().new_commit_among(&candidates, &base)?;

    assert_eq!(found, Some(newer[..7].to_owned()));
    Ok(())
  }

  #[test]
  fn should_be_none_when_no_candidate_is_new() -> Result<()> {
    let fixture = Fixture::new();
    let base = fixture.commit("a.txt", "a", "first");

    let found = fixture
      .repo()
      .new_commit_among(&[base[..7].to_owned()], &base)?;

    assert_eq!(found, None);
    Ok(())
  }
}

mod head_advanced_cleanly_from {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let base = fixture.commit("a.txt", "a", "first");
    fixture.commit("b.txt", "b", "second");

    assert!(fixture.repo().head_advanced_cleanly_from(&base)?);
    Ok(())
  }

  #[test]
  fn should_be_false_when_head_is_still_the_base() -> Result<()> {
    let fixture = Fixture::new();
    let base = fixture.commit("a.txt", "a", "first");

    assert!(!fixture.repo().head_advanced_cleanly_from(&base)?);
    Ok(())
  }

  #[test]
  fn should_be_false_when_the_base_is_not_behind_head() -> Result<()> {
    let fixture = Fixture::new();
    fixture.commit("a.txt", "a", "first");
    let other = fixture.commit("b.txt", "b", "second");
    git(&fixture.dir, &["reset", "-q", "--hard", "HEAD~1"]);

    assert!(!fixture.repo().head_advanced_cleanly_from(&other)?);
    Ok(())
  }

  #[test]
  fn should_be_false_when_the_tree_is_dirty() -> Result<()> {
    let fixture = Fixture::new();
    let base = fixture.commit("a.txt", "a", "first");
    fixture.commit("b.txt", "b", "second");
    fs::write(fixture.dir.join("c.txt"), "c")?;

    assert!(!fixture.repo().head_advanced_cleanly_from(&base)?);
    Ok(())
  }
}

mod status {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    fixture.commit("a.txt", "a", "first");
    fs::write(fixture.dir.join("a.txt"), "changed")?;
    fs::write(fixture.dir.join("new.txt"), "new")?;

    assert_eq!(fixture.repo().status()?, "M a.txt\n?? new.txt");
    assert!(!fixture.repo().is_clean()?);
    Ok(())
  }

  #[test]
  fn should_be_empty_when_the_tree_is_clean() -> Result<()> {
    let fixture = Fixture::new();
    fixture.commit("a.txt", "a", "first");

    assert_eq!(fixture.repo().status()?, "");
    assert!(fixture.repo().is_clean()?);
    Ok(())
  }
}

mod commit {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let sha = fixture.commit("a.txt", "a", "first\n\nCo-authored-by: someone");

    let commit = fixture.repo().commit(&sha[..7])?;

    assert_eq!(
      commit,
      Some(Commit {
        sha,
        message: "first\n\nCo-authored-by: someone".to_owned(),
      })
    );
    Ok(())
  }

  #[test]
  fn should_be_none_when_no_commit_has_the_sha() -> Result<()> {
    let fixture = Fixture::new();
    fixture.commit("a.txt", "a", "first");

    assert_eq!(fixture.repo().commit("0123456789abcdef")?, None);
    Ok(())
  }
}

mod shortstat {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    fixture.commit("a.txt", "a\nb\n", "first");
    let sha = fixture.commit("a.txt", "a\nc\nd\n", "second");

    assert_eq!(
      fixture.repo().shortstat(&sha)?,
      "1 file changed, 2 insertions(+), 1 deletion(-)"
    );
    Ok(())
  }
}

mod files_changed {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let fixture = Fixture::new();
    let first = fixture.commit("a.txt", "a", "first");
    fixture.commit("b.txt", "b", "second");
    let third = fixture.commit("c.txt", "c", "third");

    assert_eq!(
      fixture.repo().files_changed(&first, &third)?,
      vec!["b.txt", "c.txt"]
    );
    Ok(())
  }

  #[test]
  fn should_be_empty_when_nothing_changed() -> Result<()> {
    let fixture = Fixture::new();
    let first = fixture.commit("a.txt", "a", "first");

    assert_eq!(
      fixture.repo().files_changed(&first, "HEAD")?,
      Vec::<String>::new()
    );
    Ok(())
  }
}
