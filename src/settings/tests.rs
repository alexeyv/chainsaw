use super::*;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
    fs::write(dir.path().join(FILE_NAME), "prompt-landing-seconds = 3\n").unwrap();

    let settings = Settings::load(dir.path()).unwrap();

    assert_eq!(settings.prompt_landing_seconds(), 3);
  }

  #[test]
  fn should_use_defaults_when_the_file_is_absent() {
    let dir = ScratchDir::new();

    let settings = Settings::load(dir.path()).unwrap();

    assert_eq!(settings, Settings::default());
  }

  #[test]
  fn should_fail_when_a_retired_chainsaw_json_is_present() {
    let dir = ScratchDir::new();
    fs::write(dir.path().join("chainsaw.json"), "{}").unwrap();

    let error = Settings::load(dir.path()).unwrap_err();

    assert_eq!(
      error.to_string(),
      format!(
        "{} is no longer read; move its settings to {} as TOML and delete it",
        dir.path().join("chainsaw.json").display(),
        dir.path().join(FILE_NAME).display()
      )
    );
  }

  #[test]
  fn should_fail_naming_the_file_when_it_is_invalid() {
    let dir = ScratchDir::new();
    fs::write(dir.path().join(FILE_NAME), "nope").unwrap();

    let error = Settings::load(dir.path()).unwrap_err();

    assert!(format!("{error:#}").contains(&format!(
      "invalid settings in {}",
      dir.path().join(FILE_NAME).display()
    )));
  }
}

mod parse {
  use super::*;

  #[test]
  fn should_work() {
    let settings = Settings::parse("prompt-landing-seconds = -1\n").unwrap();

    assert_eq!(settings.prompt_landing_seconds(), -1);
  }

  #[test]
  fn should_parse_per_role_agent_clis() {
    let settings = Settings::parse(
      r#"
        [agents.lead]
        cli = "cursor"
        model = "composer-2"

        [agents.implementer]
        cli = "codex"
        model = "gpt-5.4"
        args = ["--full-auto"]

        [agents.commentator]
        cli = "claude"
        model = "sonnet"
      "#,
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
    let error = Settings::parse("[agents.reviewer]\ncli = \"claude\"\n").unwrap_err();
    assert_eq!(
      error.to_string(),
      r#"unknown agent role "reviewer"; expected lead, implementer, or commentator"#
    );
  }

  #[test]
  fn should_use_defaults_when_the_file_is_empty() {
    assert_eq!(Settings::parse("").unwrap(), Settings::default());
  }

  #[test]
  fn should_fail_when_agents_is_not_a_table() {
    let error = Settings::parse("agents = 1\n").unwrap_err();
    assert_eq!(error.to_string(), r#"setting "agents" must be a table"#);
  }

  #[test]
  fn should_fail_when_a_key_is_unknown() {
    let error = Settings::parse("prompt-landing-secnds = 1\n").unwrap_err();
    assert_eq!(
      error.to_string(),
      r#"unknown setting "prompt-landing-secnds""#
    );
  }

  #[test]
  fn should_fail_when_a_value_is_not_an_integer() {
    let error = Settings::parse("prompt-landing-seconds = \"15\"\n").unwrap_err();
    assert_eq!(
      error.to_string(),
      r#"setting "prompt-landing-seconds" must be an integer, got "15""#
    );
  }

  #[test]
  fn should_fail_when_the_text_is_not_toml() {
    assert!(Settings::parse("nope").is_err());
  }
}
