use super::*;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::agent::AgentSpec;
use crate::domain::Role;

struct ScratchDir(PathBuf);

impl ScratchDir {
  fn new() -> Self {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
      "chainsaw-settings-{}-{}",
      std::process::id(),
      COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    Self(path)
  }

  fn path(&self) -> &Path {
    &self.0
  }
}

impl Drop for ScratchDir {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

mod load {
  use super::*;

  #[test]
  fn should_work() {
    let dir = ScratchDir::new();
    fs::write(
      dir.path().join(FILE_NAME),
      r#"{"prompt-landing-seconds": 3}"#,
    )
    .unwrap();

    let settings = load_from(dir.path(), None).unwrap();

    assert_eq!(settings.prompt_landing_seconds(), 3);
  }

  #[test]
  fn should_use_defaults_when_the_file_is_absent() {
    let dir = ScratchDir::new();

    let settings = load_from(dir.path(), None).unwrap();

    assert_eq!(settings, Settings::default());
  }

  #[test]
  fn should_apply_the_global_file_when_the_run_file_is_absent() {
    let global = ScratchDir::new();
    let run = ScratchDir::new();
    fs::write(
      global.path().join(FILE_NAME),
      r#"{"agents":{"implementer":{"cli":"cursor","model":"composer-2.5"}}}"#,
    )
    .unwrap();

    let settings = load_from(run.path(), Some(&global.path().join(FILE_NAME))).unwrap();

    assert_eq!(settings.agent(Role::Implementer).cli().as_str(), "cursor");
    assert_eq!(
      settings.agent(Role::Implementer).model(),
      Some("composer-2.5")
    );
    assert_eq!(settings.agent(Role::Lead), &AgentSpec::claude_opus());
  }

  #[test]
  fn should_let_the_run_file_override_named_roles() {
    let global = ScratchDir::new();
    let run = ScratchDir::new();
    fs::write(
      global.path().join(FILE_NAME),
      r#"{
        "prompt-landing-seconds": 9,
        "agents": {
          "lead": {"cli": "claude", "model": "opus"},
          "implementer": {"cli": "cursor", "model": "composer-2.5"}
        }
      }"#,
    )
    .unwrap();
    fs::write(
      run.path().join(FILE_NAME),
      r#"{"agents":{"implementer":{"cli":"codex","model":"gpt-5.3-codex"}}}"#,
    )
    .unwrap();

    let settings = load_from(run.path(), Some(&global.path().join(FILE_NAME))).unwrap();

    assert_eq!(settings.prompt_landing_seconds(), 9);
    assert_eq!(settings.agent(Role::Lead).model(), Some("opus"));
    assert_eq!(settings.agent(Role::Implementer).cli().as_str(), "codex");
    assert_eq!(
      settings.agent(Role::Implementer).model(),
      Some("gpt-5.3-codex")
    );
  }

  #[test]
  fn should_fail_naming_the_file_when_it_is_invalid() {
    let dir = ScratchDir::new();
    fs::write(dir.path().join(FILE_NAME), "[]").unwrap();

    let error = load_from(dir.path(), None).unwrap_err();

    assert!(format!("{error:#}").contains(&format!(
      "invalid settings in {}",
      dir.path().join(FILE_NAME).display()
    )));
  }

  #[test]
  fn should_fail_naming_the_global_file_when_it_is_invalid() {
    let global = ScratchDir::new();
    let run = ScratchDir::new();
    let path = global.path().join(FILE_NAME);
    fs::write(&path, "[]").unwrap();

    let error = load_from(run.path(), Some(&path)).unwrap_err();

    assert!(format!("{error:#}").contains(&format!("invalid settings in {}", path.display())));
  }
}

mod global_config_path_from {
  use super::*;

  #[test]
  fn should_work() {
    let home = Path::new("/Users/a");

    assert_eq!(
      global_config_path_from(None, Some(home)).as_deref(),
      Some(Path::new("/Users/a/.config/chainsaw/chainsaw.json"))
    );
  }

  #[test]
  fn should_use_the_env_path_when_set() {
    assert_eq!(
      global_config_path_from(Some(OsString::from("/tmp/custom.json")), None).as_deref(),
      Some(Path::new("/tmp/custom.json"))
    );
  }

  #[test]
  fn should_disable_the_global_file_when_the_env_is_empty() {
    assert_eq!(
      global_config_path_from(Some(OsString::new()), Some(Path::new("/Users/a"))),
      None
    );
  }

  #[test]
  fn should_omit_the_global_file_when_home_is_unset() {
    assert_eq!(global_config_path_from(None, None), None);
  }
}

mod parse {
  use super::*;

  #[test]
  fn should_work() {
    let settings = Settings::parse(r#"{"prompt-landing-seconds": -1}"#).unwrap();

    assert_eq!(settings.prompt_landing_seconds(), -1);
  }

  #[test]
  fn should_parse_per_role_agent_clis() {
    let settings = Settings::parse(
      r#"{
        "agents": {
          "lead": {"cli": "cursor", "model": "composer-2"},
          "implementer": {"cli": "codex", "model": "gpt-5.4", "args": ["--full-auto"]},
          "commentator": {"cli": "claude", "model": "sonnet"}
        }
      }"#,
    )
    .unwrap();

    assert_eq!(settings.agent(Role::Lead).cli().as_str(), "cursor");
    assert_eq!(settings.agent(Role::Lead).model(), Some("composer-2"));
    assert_eq!(settings.agent(Role::Implementer).cli().as_str(), "codex");
    assert_eq!(settings.agent(Role::Implementer).model(), Some("gpt-5.4"));
    assert_eq!(
      settings.agent(Role::Implementer).args(),
      &["--full-auto".to_owned()]
    );
    assert_eq!(settings.agent(Role::Commentator).model(), Some("sonnet"));
  }

  #[test]
  fn should_fail_when_an_agent_role_is_unknown() {
    let error = Settings::parse(r#"{"agents":{"reviewer":{"cli":"claude"}}}"#).unwrap_err();
    assert_eq!(error.to_string(), r#"unknown agent role "reviewer""#);
  }

  #[test]
  fn should_use_defaults_when_the_object_is_empty() {
    assert_eq!(Settings::parse("{}").unwrap(), Settings::default());
  }

  #[test]
  fn should_fail_when_the_top_level_is_not_an_object() {
    let error = Settings::parse("[1]").unwrap_err();
    assert_eq!(error.to_string(), "expected a JSON object at the top level");
  }

  #[test]
  fn should_fail_when_a_key_is_unknown() {
    let error = Settings::parse(r#"{"prompt-landing-secnds": 1}"#).unwrap_err();
    assert_eq!(
      error.to_string(),
      r#"unknown setting "prompt-landing-secnds""#
    );
  }

  #[test]
  fn should_fail_when_a_value_is_not_an_integer() {
    let error = Settings::parse(r#"{"prompt-landing-seconds": "15"}"#).unwrap_err();
    assert_eq!(
      error.to_string(),
      r#"setting "prompt-landing-seconds" must be an integer, got "15""#
    );
  }

  #[test]
  fn should_fail_when_the_text_is_not_json() {
    assert!(Settings::parse("nope").is_err());
  }
}
