use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use super::*;

/// Each symlink invokes the checked-in shell script with its own directory
/// for the call log. All tests can run concurrently.
pub(super) struct FakeCli {
  pub(super) dir: PathBuf,
  pub(super) program: PathBuf,
}

impl FakeCli {
  pub(super) fn new(name: &str) -> Self {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
      std::env::temp_dir().join(format!("chainsaw-{name}-{}-{sequence}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let program = dir.join(name);
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("src/infra/session_runtime")
      .join(name)
      .join("fake.sh");
    std::os::unix::fs::symlink(script, &program).unwrap();
    Self { dir, program }
  }

  pub(super) fn calls(&self) -> Vec<Vec<String>> {
    let mut calls = Vec::new();
    let mut current = Vec::new();
    for line in fs::read_to_string(self.dir.join("calls"))
      .unwrap_or_default()
      .lines()
    {
      if line == "END-OF-CALL" {
        calls.push(std::mem::take(&mut current));
      } else {
        current.push(line.to_owned());
      }
    }
    calls
  }
}

impl Drop for FakeCli {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.dir);
  }
}

mod run {
  use super::*;

  #[test]
  fn should_work() {
    let cli = Cli::new("echo", "echo");

    let output = cli.run(&["hello"]).unwrap();

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\n");
  }

  #[test]
  fn should_capture_the_output_when_the_program_fails() {
    let cli = Cli::new("false", "false");

    let output = cli.run(&[]).unwrap();

    assert!(!output.status.success());
  }

  #[test]
  fn should_fail_when_the_program_is_missing() {
    let cli = Cli::new("nowhere", "/nonexistent/nowhere");

    let error = cli.run(&[]).unwrap_err();

    assert_eq!(error.to_string(), "failed to run /nonexistent/nowhere");
  }
}

mod json_string {
  use super::*;

  #[test]
  fn should_work() {
    let cli = Cli::new("orca", "orca");
    let reply = json!({"result": {"handle": "t-1"}});

    assert_eq!(cli.json_string(&reply, "/result/handle").unwrap(), "t-1");
  }

  #[test]
  fn should_fail_when_the_pointer_is_missing() {
    let cli = Cli::new("orca", "orca");
    let reply = json!({"result": {}});

    let error = cli.json_string(&reply, "/result/handle").unwrap_err();

    assert_eq!(error.to_string(), "orca response lacks /result/handle");
  }

  #[test]
  fn should_fail_when_the_value_is_not_a_string() {
    let cli = Cli::new("herdr", "herdr");
    let reply = json!({"result": {"handle": 7}});

    let error = cli.json_string(&reply, "/result/handle").unwrap_err();

    assert_eq!(error.to_string(), "herdr response lacks /result/handle");
  }
}
