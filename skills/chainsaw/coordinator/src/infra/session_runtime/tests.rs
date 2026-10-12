use serde_json::json;

use super::*;

mod run {
  use super::*;

  #[test]
  fn should_work() {
    // git runs on every platform chainsaw does, as chainsaw itself needs it.
    let cli = Cli::new("git", "git");

    let output = cli.run(&["--version"]).unwrap();

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("git version "));
  }

  #[test]
  fn should_capture_the_output_when_the_program_fails() {
    let cli = Cli::new("git", "git");

    let output = cli.run(&["no-such-command"]).unwrap();

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
