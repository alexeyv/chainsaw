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

  fn write_settings(&self, text: &str) {
    fs::write(self.path().join(FILE_NAME), text).unwrap();
  }
}

impl Drop for ScratchDir {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

const DISALLOWED: &str = "WebSearch,WebFetch,NotebookEdit,Task,Agent,AskUserQuestion,EnterPlanMode,ExitPlanMode,TaskOutput";

fn sets(values: &[&str]) -> Vec<String> {
  values.iter().map(|value| (*value).to_owned()).collect()
}

/// Serde's messages end in a newline; the CLI trims it, so the tests do too.
fn message(error: &anyhow::Error) -> String {
  error.to_string().trim_end().to_owned()
}

fn table(text: &str) -> Table {
  text.parse().unwrap()
}

fn load_text(text: &str) -> Result<Settings> {
  let dir = ScratchDir::new();
  dir.write_settings(text);
  Settings::load(dir.path(), &[])
}

fn load_sets(values: &[&str]) -> Result<Settings> {
  let dir = ScratchDir::new();
  Settings::load(dir.path(), &sets(values))
}

mod load {
  use super::*;

  #[test]
  fn should_work() {
    let dir = ScratchDir::new();
    dir.write_settings(
      r#"
prompt-landing-seconds = 3

[implementer]
args = "--model sonnet --effort medium"

[commentator]
"#,
    );

    let settings = Settings::load(dir.path(), &sets(&["commentator.args=--model haiku"])).unwrap();

    assert_eq!(settings.prompt_landing(), Duration::from_secs(3));
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--model", "sonnet", "--effort", "medium"]
    );
    assert_eq!(
      settings.launch_args(SessionKind::Commentator),
      ["--model", "haiku"]
    );
  }

  #[test]
  fn should_use_todays_exact_lists_when_the_file_is_absent_and_nothing_is_set() {
    let dir = ScratchDir::new();

    let settings = Settings::load(dir.path(), &[]).unwrap();

    assert_eq!(settings.prompt_landing(), Duration::from_secs(15));
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      [
        "--model",
        "opus",
        "--effort",
        "high",
        "--disable-slash-commands",
        "--strict-mcp-config",
        "--no-chrome",
        "--disallowedTools",
        DISALLOWED,
      ]
    );
    assert_eq!(
      settings.launch_args(SessionKind::Commentator),
      [
        "--model",
        "opus",
        "--effort",
        "high",
        "--strict-mcp-config",
        "--no-chrome",
        "--disallowedTools",
        DISALLOWED,
      ]
    );
  }

  #[test]
  fn should_keep_defaults_when_the_file_is_empty() {
    let dir = ScratchDir::new();

    assert_eq!(
      load_text("").unwrap(),
      Settings::load(dir.path(), &[]).unwrap()
    );
  }

  #[test]
  fn should_keep_defaults_for_a_role_given_an_empty_table() {
    let dir = ScratchDir::new();

    assert_eq!(
      load_text("[implementer]\n").unwrap(),
      Settings::load(dir.path(), &[]).unwrap()
    );
  }

  #[test]
  fn should_pass_args_through_untouched_apart_from_splitting() {
    let settings = load_text("[implementer]\nargs = \"  --whatever=1   x  \"\n").unwrap();

    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--whatever=1", "x"]
    );
  }

  #[test]
  fn should_keep_a_quoted_value_with_spaces_as_one_arg() {
    let settings =
      load_text("[implementer]\nargs = \"--append-system-prompt 'be terse' --model sonnet\"\n")
        .unwrap();

    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--append-system-prompt", "be terse", "--model", "sonnet"]
    );
  }

  #[test]
  fn should_launch_with_no_flags_when_args_is_empty() {
    let settings = load_text("[commentator]\nargs = \"\"\n").unwrap();

    assert!(settings.launch_args(SessionKind::Commentator).is_empty());
  }

  #[test]
  fn should_let_a_set_beat_the_file() {
    let dir = ScratchDir::new();
    dir.write_settings("[implementer]\nargs = \"--chrome\"\n");

    let settings =
      Settings::load(dir.path(), &sets(&["implementer.args=--effort medium"])).unwrap();

    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--effort", "medium"]
    );
  }

  #[test]
  fn should_fail_naming_the_file_and_the_cause_when_a_key_is_unknown() {
    let error = load_text("promt-landing-seconds = 1\n").unwrap_err();

    assert_eq!(
      message(&error),
      "invalid settings in chainsaw.toml: unknown field `promt-landing-seconds`, expected one of `prompt-landing-seconds`, `implementer`, `commentator`"
    );
  }

  #[test]
  fn should_fail_naming_the_role_when_a_nested_key_is_unknown() {
    let error = load_text("[implementer]\nmodel = \"x\"\n").unwrap_err();

    assert_eq!(
      message(&error),
      "invalid settings in chainsaw.toml: unknown field `model`, expected `args`\nin `implementer`"
    );
  }

  #[test]
  fn should_fail_when_a_role_is_unknown() {
    let error = load_text("[reviewer]\nargs = \"x\"\n").unwrap_err();

    assert!(message(&error).contains("unknown field `reviewer`, expected one of"));
  }

  #[test]
  fn should_fail_when_a_value_has_the_wrong_type() {
    let error = load_text("[implementer]\nargs = [\"--chrome\"]\n").unwrap_err();

    assert_eq!(
      message(&error),
      "invalid settings in chainsaw.toml: invalid type: sequence, expected a string\nin `implementer.args`"
    );
  }

  #[test]
  fn should_fail_naming_the_role_when_a_quote_is_unbalanced() {
    let error =
      load_text("[commentator]\nargs = \"--append-system-prompt 'be terse\"\n").unwrap_err();

    assert_eq!(
      message(&error),
      "invalid settings in chainsaw.toml: missing closing quote\nin `commentator.args`"
    );
  }

  #[test]
  fn should_fail_when_an_integer_is_negative() {
    let error = load_text("prompt-landing-seconds = -1\n").unwrap_err();

    assert_eq!(
      message(&error),
      "invalid settings in chainsaw.toml: invalid value: integer `-1`, expected u64\nin `prompt-landing-seconds`"
    );
  }

  #[test]
  fn should_fail_naming_the_file_when_it_is_not_toml() {
    let error = load_text("[implementer\n").unwrap_err();

    assert!(message(&error).starts_with("invalid settings in chainsaw.toml: TOML parse error"));
  }

  #[test]
  fn should_fail_naming_the_file_and_the_cause_when_it_cannot_be_read() {
    let dir = ScratchDir::new();
    fs::create_dir(dir.path().join(FILE_NAME)).unwrap();

    let error = Settings::load(dir.path(), &[]).unwrap_err();

    let prefix = "cannot read chainsaw.toml: ";
    assert!(message(&error).starts_with(prefix));
    assert!(message(&error).len() > prefix.len());
  }

  #[test]
  fn should_fail_naming_the_set_and_the_cause_when_it_is_invalid() {
    let error = load_sets(&["implementer.model=x"]).unwrap_err();

    assert_eq!(
      message(&error),
      "invalid --set implementer.model=x: unknown field `model`, expected `args`\nin `implementer`"
    );
  }

  #[test]
  fn should_fail_naming_the_set_when_its_quote_is_unbalanced() {
    let error = load_sets(&["implementer.args=--model \"sonnet"]).unwrap_err();

    assert_eq!(
      message(&error),
      "invalid --set implementer.args=--model \"sonnet: missing closing quote\nin `implementer.args`"
    );
  }

  #[test]
  fn should_fail_naming_the_key_when_it_is_set_twice() {
    let error = load_sets(&["prompt-landing-seconds=1", "prompt-landing-seconds=2"]).unwrap_err();

    assert_eq!(
      message(&error),
      "invalid --set prompt-landing-seconds=2: prompt-landing-seconds was already set by an earlier --set"
    );
  }

  #[test]
  fn should_fail_telling_the_human_to_move_a_leftover_chainsaw_json() {
    let dir = ScratchDir::new();
    fs::write(dir.path().join("chainsaw.json"), "{}").unwrap();

    let error = Settings::load(dir.path(), &[]).unwrap_err();

    assert_eq!(
      message(&error),
      "chainsaw.json is no longer used; transfer its settings to chainsaw.toml and delete"
    );
  }
}

mod set_over {
  use super::*;

  fn apply(text: &str, set: &str) -> Result<Table> {
    set_over(table(text), set, &[])
  }

  #[test]
  fn should_work() {
    let result = apply("prompt-landing-seconds = 15\n", "prompt-landing-seconds=20").unwrap();

    assert_eq!(result["prompt-landing-seconds"], Value::Integer(20));
  }

  #[test]
  fn should_take_a_bare_word_as_a_string() {
    let result = apply("", "implementer.args=--chrome").unwrap();

    assert_eq!(result["implementer"]["args"].as_str(), Some("--chrome"));
  }

  #[test]
  fn should_take_words_with_spaces_as_a_string() {
    let result = apply("", "implementer.args=--effort medium").unwrap();

    assert_eq!(
      result["implementer"]["args"].as_str(),
      Some("--effort medium")
    );
  }

  #[test]
  fn should_take_a_quoted_string_without_its_quotes() {
    let result = apply("", "implementer.args=\"--chrome\"").unwrap();

    assert_eq!(result["implementer"]["args"].as_str(), Some("--chrome"));
  }

  #[test]
  fn should_keep_the_rest_of_the_file() {
    let result = apply(
      "prompt-landing-seconds = 3\n[implementer]\nargs = \"--chrome\"\n",
      "implementer.args=--effort medium",
    )
    .unwrap();

    assert_eq!(result["prompt-landing-seconds"], Value::Integer(3));
    assert_eq!(
      result["implementer"]["args"].as_str(),
      Some("--effort medium")
    );
  }

  #[test]
  fn should_fail_when_there_is_no_equals_sign() {
    let error = apply("", "prompt-landing-seconds").unwrap_err();

    assert_eq!(message(&error), "expected KEY=VALUE");
  }

  #[test]
  fn should_fail_when_the_key_is_empty() {
    let error = apply("", "=5").unwrap_err();

    assert!(message(&error).contains("unquoted keys cannot be empty"));
  }

  #[test]
  fn should_fail_when_an_earlier_set_named_the_key() {
    let error = set_over(
      table(""),
      "implementer.args=b",
      &sets(&["implementer.args=a"]),
    )
    .unwrap_err();

    assert_eq!(
      message(&error),
      "implementer.args was already set by an earlier --set"
    );
  }
}

mod merge {
  use super::*;

  #[test]
  fn should_work() {
    let merged = merge(
      table("a = 1\n[t]\nx = 1\ny = 1\n"),
      table("b = 2\n[t]\ny = 2\n"),
    );

    assert_eq!(
      merged.to_string(),
      table("a = 1\nb = 2\n[t]\nx = 1\ny = 2\n").to_string()
    );
  }

  #[test]
  fn should_replace_a_scalar_with_a_table() {
    let merged = merge(table("t = 1\n"), table("[t]\nx = 1\n"));

    assert_eq!(merged["t"]["x"], Value::Integer(1));
  }
}
