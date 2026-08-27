use super::*;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

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
      r#"{"prompt-landing-seconds": 3, "reuse-max-context": 48000, "reuse-max-stale-lines": 50}"#,
    )
    .unwrap();

    let settings = Settings::load(dir.path()).unwrap();

    assert_eq!(settings.prompt_landing_seconds(), 3);
    assert_eq!(settings.reuse_max_context(), 48_000);
    assert_eq!(settings.reuse_max_stale_lines(), 50);
  }

  #[test]
  fn should_use_defaults_when_the_file_is_absent() {
    let dir = ScratchDir::new();

    let settings = Settings::load(dir.path()).unwrap();

    assert_eq!(settings, Settings::default());
  }

  #[test]
  fn should_fail_naming_the_file_when_it_is_invalid() {
    let dir = ScratchDir::new();
    fs::write(dir.path().join(FILE_NAME), "[]").unwrap();

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
    let settings = Settings::parse(r#"{"reuse-max-context": -1}"#).unwrap();

    assert_eq!(
      settings.prompt_landing_seconds(),
      DEFAULT_PROMPT_LANDING_SECONDS
    );
    assert_eq!(settings.reuse_max_context(), -1);
    assert_eq!(
      settings.reuse_max_stale_lines(),
      DEFAULT_REUSE_MAX_STALE_LINES
    );
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
    let error = Settings::parse(r#"{"reuse-max-contxt": 1}"#).unwrap_err();
    assert_eq!(error.to_string(), r#"unknown setting "reuse-max-contxt""#);
  }

  #[test]
  fn should_fail_when_a_value_is_not_an_integer() {
    let error = Settings::parse(r#"{"reuse-max-context": "48000"}"#).unwrap_err();
    assert_eq!(
      error.to_string(),
      r#"setting "reuse-max-context" must be an integer, got "48000""#
    );
  }

  #[test]
  fn should_fail_when_the_text_is_not_json() {
    assert!(Settings::parse("nope").is_err());
  }
}
