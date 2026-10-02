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

  /// Where the global file would be under a home directory in the scratch
  fn global_file(&self) -> PathBuf {
    self
      .path()
      .join("home")
      .join(".config")
      .join("chainsaw")
      .join(FILE_NAME)
  }

  fn write_global(&self, text: &str) {
    fs::create_dir_all(self.global_file().parent().unwrap()).unwrap();
    fs::write(self.global_file(), text).unwrap();
  }

  fn write_settings(&self, text: &str) {
    fs::write(self.path().join(FILE_NAME), text).unwrap();
  }

  fn write_local(&self, text: &str) {
    fs::write(self.path().join(LOCAL_FILE_NAME), text).unwrap();
  }

  /// Loads with the scratch as the run directory and its global file
  fn load(&self, values: &[&str]) -> Result<Settings> {
    Settings::load_from(Some(&self.global_file()), self.path(), &sets(values))
  }

  /// Loads with the scratch as the run directory and no global file
  fn load_without_global(&self, values: &[&str]) -> Result<Settings> {
    Settings::load_from(None, self.path(), &sets(values))
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
  dir.load(&[])
}

fn load_sets(values: &[&str]) -> Result<Settings> {
  ScratchDir::new().load(values)
}

mod load {
  use super::*;

  #[test]
  fn should_work() {
    let dir = ScratchDir::new();
    dir.write_settings(
      r#"
prompt-timeout-seconds = 3

[implementer]
agent = "claude"
args = "--model sonnet --effort medium"

[commentator]
"#,
    );

    let settings = dir.load(&["commentator.args=--model haiku"]).unwrap();

    assert_eq!(settings.prompt_timeout(), Duration::from_secs(3));
    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Claude
    );
    assert_eq!(
      settings.launch_agent(SessionKind::Commentator),
      AgentKind::Claude
    );
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

    let settings = dir.load(&[]).unwrap();

    assert_eq!(settings.prompt_timeout(), Duration::from_secs(15));
    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Claude
    );
    assert_eq!(
      settings.launch_agent(SessionKind::Commentator),
      AgentKind::Claude
    );
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

    assert_eq!(load_text("").unwrap(), dir.load(&[]).unwrap());
  }

  #[test]
  fn should_keep_defaults_for_a_role_given_an_empty_table() {
    let dir = ScratchDir::new();

    assert_eq!(
      load_text("[implementer]\n").unwrap(),
      dir.load(&[]).unwrap()
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

    let settings = dir.load(&["implementer.args=--effort medium"]).unwrap();

    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--effort", "medium"]
    );
  }

  #[test]
  fn should_read_the_global_file_when_the_run_directory_has_none() {
    let dir = ScratchDir::new();
    dir.write_global("prompt-timeout-seconds = 4\n[implementer]\nagent = \"codex\"\n");

    let settings = dir.load(&[]).unwrap();

    assert_eq!(settings.prompt_timeout(), Duration::from_secs(4));
    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Codex
    );
  }

  #[test]
  fn should_skip_the_global_layer_when_there_is_no_global_file() {
    let dir = ScratchDir::new();
    dir.write_global("prompt-timeout-seconds = 4\n");
    dir.write_settings("[implementer]\nargs = \"--project\"\n");

    let settings = dir.load_without_global(&[]).unwrap();

    assert_eq!(settings.prompt_timeout(), Duration::from_secs(15));
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--project"]
    );
  }

  #[test]
  fn should_lay_local_over_project_over_global_key_by_key() {
    let dir = ScratchDir::new();
    dir.write_global(
      "prompt-timeout-seconds = 4\n[implementer]\nargs = \"--global\"\n[commentator]\nagent = \"codex\"\nargs = \"--global\"\n",
    );
    dir
      .write_settings("[implementer]\nargs = \"--project\"\n[commentator]\nargs = \"--project\"\n");
    dir.write_local("[implementer]\nargs = \"--local\"\n");

    let settings = dir.load(&[]).unwrap();

    assert_eq!(settings.prompt_timeout(), Duration::from_secs(4));
    assert_eq!(settings.launch_args(SessionKind::Implementer), ["--local"]);
    assert_eq!(
      settings.launch_agent(SessionKind::Commentator),
      AgentKind::Codex
    );
    assert_eq!(
      settings.launch_args(SessionKind::Commentator),
      ["--project"]
    );
  }

  #[test]
  fn should_drop_inherited_args_when_a_later_file_names_the_agent() {
    let dir = ScratchDir::new();
    dir.write_global("[implementer]\nargs = \"--model sonnet --effort medium\"\n");
    dir.write_settings("[implementer]\nagent = \"codex\"\n");

    let settings = dir.load(&[]).unwrap();

    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Codex
    );
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--dangerously-bypass-approvals-and-sandbox"]
    );
  }

  #[test]
  fn should_keep_the_later_args_when_a_later_file_names_the_agent_and_its_args() {
    let dir = ScratchDir::new();
    dir.write_global("[implementer]\nargs = \"--global\"\n");
    dir.write_settings("[implementer]\nagent = \"codex\"\nargs = \"--project\"\n");

    let settings = dir.load(&[]).unwrap();

    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Codex
    );
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--project"]
    );
  }

  #[test]
  fn should_keep_inherited_args_when_an_earlier_file_names_the_agent() {
    let dir = ScratchDir::new();
    dir.write_global("[implementer]\nagent = \"codex\"\n");
    dir.write_settings("[implementer]\nargs = \"--project\"\n");

    let settings = dir.load(&[]).unwrap();

    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Codex
    );
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--project"]
    );
  }

  #[test]
  fn should_drop_the_files_args_when_a_set_names_the_agent() {
    let dir = ScratchDir::new();
    dir.write_settings("[implementer]\nargs = \"--model sonnet\"\n");

    let settings = dir.load(&["implementer.agent=codex"]).unwrap();

    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Codex
    );
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--dangerously-bypass-approvals-and-sandbox"]
    );
  }

  #[test]
  fn should_launch_codex_with_the_set_args_when_the_agent_is_set_first() {
    let settings = load_sets(&[
      "implementer.agent=codex",
      "implementer.args=--model gpt-5.4",
    ])
    .unwrap();

    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Codex
    );
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--model", "gpt-5.4"]
    );
  }

  #[test]
  fn should_launch_codex_with_the_set_args_when_the_args_are_set_first() {
    let settings = load_sets(&[
      "implementer.args=--model gpt-5.4",
      "implementer.agent=codex",
    ])
    .unwrap();

    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Codex
    );
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--model", "gpt-5.4"]
    );
  }

  #[test]
  fn should_let_a_set_beat_the_local_file() {
    let dir = ScratchDir::new();
    dir.write_local("[implementer]\nargs = \"--local\"\n");

    let settings = dir.load(&["implementer.args=--set"]).unwrap();

    assert_eq!(settings.launch_args(SessionKind::Implementer), ["--set"]);
  }

  #[test]
  fn should_fail_naming_the_global_file_by_its_full_path_when_it_is_invalid() {
    let dir = ScratchDir::new();
    dir.write_global("[implementer]\nmodel = \"x\"\n");

    let error = dir.load(&[]).unwrap_err();

    assert_eq!(
      message(&error),
      format!(
        "invalid settings in {}: unknown field `model`, expected `agent` or `args`\nin `implementer`",
        dir.global_file().display()
      )
    );
  }

  #[test]
  fn should_fail_naming_the_local_file_when_it_is_invalid_even_though_the_project_file_is_fine() {
    let dir = ScratchDir::new();
    dir.write_settings("[implementer]\nargs = \"--project\"\n");
    dir.write_local("[implementer]\nargs = 1\n");

    let error = dir.load(&[]).unwrap_err();

    assert_eq!(
      message(&error),
      "invalid settings in chainsaw.local.toml: invalid type: integer `1`, expected a string\nin `implementer.args`"
    );
  }

  #[test]
  fn should_launch_codex_with_its_own_defaults_when_a_role_names_it() {
    let settings = load_text("[implementer]\nagent = \"codex\"\n").unwrap();

    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Codex
    );
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--dangerously-bypass-approvals-and-sandbox"]
    );
    assert_eq!(
      settings.launch_agent(SessionKind::Commentator),
      AgentKind::Claude
    );
  }

  #[test]
  fn should_launch_cursor_with_its_own_defaults_when_a_role_names_it() {
    let settings = load_text("[implementer]\nagent = \"cursor\"\n").unwrap();

    assert_eq!(
      settings.launch_agent(SessionKind::Implementer),
      AgentKind::Cursor
    );
    assert_eq!(
      settings.launch_args(SessionKind::Implementer),
      ["--trust", "--force"]
    );
    assert_eq!(
      settings.launch_agent(SessionKind::Commentator),
      AgentKind::Claude
    );
  }

  #[test]
  fn should_let_a_set_name_the_agent() {
    let settings = load_sets(&["commentator.agent=codex"]).unwrap();

    assert_eq!(
      settings.launch_agent(SessionKind::Commentator),
      AgentKind::Codex
    );
    assert_eq!(
      settings.launch_args(SessionKind::Commentator),
      ["--dangerously-bypass-approvals-and-sandbox"]
    );
  }

  #[test]
  fn should_fail_naming_the_role_the_value_and_the_accepted_agents_when_the_agent_is_unknown() {
    let error = load_text("[implementer]\nagent = \"gemini\"\n").unwrap_err();

    assert_eq!(
      message(&error),
      "invalid settings in chainsaw.toml: unknown agent \"gemini\", expected one of `claude`, `codex`, `cursor`\nin `implementer.agent`"
    );
  }

  #[test]
  fn should_fail_naming_the_set_when_it_names_an_unknown_agent() {
    let error = load_sets(&["commentator.agent=gemini"]).unwrap_err();

    assert_eq!(
      message(&error),
      "invalid --set commentator.agent=gemini: unknown agent \"gemini\", expected one of `claude`, `codex`, `cursor`\nin `commentator.agent`"
    );
  }

  #[test]
  fn should_fail_naming_the_file_and_the_cause_when_a_key_is_unknown() {
    let error = load_text("promt-timeout-seconds = 1\n").unwrap_err();

    assert_eq!(
      message(&error),
      "invalid settings in chainsaw.toml: unknown field `promt-timeout-seconds`, expected one of `prompt-timeout-seconds`, `implementer`, `commentator`"
    );
  }

  #[test]
  fn should_fail_naming_the_role_when_a_nested_key_is_unknown() {
    let error = load_text("[implementer]\nmodel = \"x\"\n").unwrap_err();

    assert_eq!(
      message(&error),
      "invalid settings in chainsaw.toml: unknown field `model`, expected `agent` or `args`\nin `implementer`"
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
    let error = load_text("prompt-timeout-seconds = -1\n").unwrap_err();

    assert_eq!(
      message(&error),
      "invalid settings in chainsaw.toml: invalid value: integer `-1`, expected u64\nin `prompt-timeout-seconds`"
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

    let error = dir.load(&[]).unwrap_err();

    let prefix = "cannot read chainsaw.toml: ";
    assert!(message(&error).starts_with(prefix));
    assert!(message(&error).len() > prefix.len());
  }

  #[test]
  fn should_fail_naming_the_set_and_the_cause_when_it_is_invalid() {
    let error = load_sets(&["implementer.model=x"]).unwrap_err();

    assert_eq!(
      message(&error),
      "invalid --set implementer.model=x: unknown field `model`, expected `agent` or `args`\nin `implementer`"
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
    let error = load_sets(&["prompt-timeout-seconds=1", "prompt-timeout-seconds=2"]).unwrap_err();

    assert_eq!(
      message(&error),
      "invalid --set prompt-timeout-seconds=2: prompt-timeout-seconds was already set by an earlier --set"
    );
  }

  #[test]
  fn should_fail_telling_the_human_to_move_a_leftover_chainsaw_json() {
    let dir = ScratchDir::new();
    fs::write(dir.path().join("chainsaw.json"), "{}").unwrap();

    let error = dir.load(&[]).unwrap_err();

    assert_eq!(
      message(&error),
      "chainsaw.json is no longer used; transfer its settings to chainsaw.toml and delete"
    );
  }
}

mod sets_layer {
  use super::*;

  #[test]
  fn should_work() {
    let layer = sets_layer(&sets(&[
      "prompt-timeout-seconds=20",
      "implementer.args=--chrome",
    ]))
    .unwrap();

    assert_eq!(
      layer.to_string(),
      "prompt-timeout-seconds = 20\n\n[implementer]\nargs = \"--chrome\"\n"
    );
  }

  #[test]
  fn should_build_the_same_layer_whichever_order_the_sets_come_in() {
    let agent_first =
      sets_layer(&sets(&["implementer.agent=codex", "implementer.args=-a"])).unwrap();
    let args_first =
      sets_layer(&sets(&["implementer.args=-a", "implementer.agent=codex"])).unwrap();

    assert_eq!(agent_first.to_string(), args_first.to_string());
    assert_eq!(
      agent_first.to_string(),
      "[implementer]\nagent = \"codex\"\nargs = \"-a\"\n"
    );
  }

  #[test]
  fn should_build_an_empty_layer_when_nothing_is_set() {
    assert_eq!(sets_layer(&[]).unwrap(), Table::new());
  }

  #[test]
  fn should_fail_naming_the_set_and_the_cause_when_it_is_invalid() {
    let error =
      sets_layer(&sets(&["prompt-timeout-seconds=1", "implementer.model=x"])).unwrap_err();

    assert_eq!(
      message(&error),
      "invalid --set implementer.model=x: unknown field `model`, expected `agent` or `args`\nin `implementer`"
    );
  }

  #[test]
  fn should_fail_naming_the_later_set_when_two_name_the_same_key() {
    let error = sets_layer(&sets(&["implementer.args=a", "implementer.args=b"])).unwrap_err();

    assert_eq!(
      message(&error),
      "invalid --set implementer.args=b: implementer.args was already set by an earlier --set"
    );
  }
}

mod set_into {
  use super::*;

  fn apply(text: &str, set: &str) -> Result<Table> {
    set_into(table(text), set, &[])
  }

  #[test]
  fn should_work() {
    let result = apply("prompt-timeout-seconds = 15\n", "prompt-timeout-seconds=20").unwrap();

    assert_eq!(result["prompt-timeout-seconds"], Value::Integer(20));
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
  fn should_keep_the_rest_of_the_layer() {
    let result = apply(
      "prompt-timeout-seconds = 3\n[implementer]\nargs = \"--chrome\"\n",
      "implementer.args=--effort medium",
    )
    .unwrap();

    assert_eq!(result["prompt-timeout-seconds"], Value::Integer(3));
    assert_eq!(
      result["implementer"]["args"].as_str(),
      Some("--effort medium")
    );
  }

  #[test]
  fn should_keep_the_layers_args_when_the_set_names_the_agent() {
    let result = apply("[implementer]\nargs = \"-a\"\n", "implementer.agent=codex").unwrap();

    assert_eq!(result["implementer"]["agent"].as_str(), Some("codex"));
    assert_eq!(result["implementer"]["args"].as_str(), Some("-a"));
  }

  #[test]
  fn should_fail_when_there_is_no_equals_sign() {
    let error = apply("", "prompt-timeout-seconds").unwrap_err();

    assert_eq!(message(&error), "expected KEY=VALUE");
  }

  #[test]
  fn should_fail_when_the_key_is_empty() {
    let error = apply("", "=5").unwrap_err();

    assert!(message(&error).contains("unquoted keys cannot be empty"));
  }

  #[test]
  fn should_fail_naming_the_cause_when_the_set_is_invalid() {
    let error = apply("", "implementer.model=x").unwrap_err();

    assert_eq!(
      message(&error),
      "unknown field `model`, expected `agent` or `args`\nin `implementer`"
    );
  }

  #[test]
  fn should_fail_when_an_earlier_set_named_the_key() {
    let error = set_into(
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

mod lay_over {
  use super::*;

  #[test]
  fn should_work() {
    let laid = lay_over(
      table("prompt-timeout-seconds = 4\n[implementer]\nargs = \"--global\"\n"),
      table("[implementer]\nargs = \"--project\"\n"),
    );

    assert_eq!(
      laid.to_string(),
      table("prompt-timeout-seconds = 4\n[implementer]\nargs = \"--project\"\n").to_string()
    );
  }

  #[test]
  fn should_drop_the_args_below_when_the_layer_names_the_agent_without_args() {
    let laid = lay_over(
      table("[implementer]\nargs = \"--global\"\n[commentator]\nargs = \"--global\"\n"),
      table("[implementer]\nagent = \"codex\"\n"),
    );

    assert_eq!(
      laid.to_string(),
      table("[implementer]\nagent = \"codex\"\n[commentator]\nargs = \"--global\"\n").to_string()
    );
  }

  #[test]
  fn should_keep_the_args_below_when_the_layer_names_neither_agent_nor_args() {
    let laid = lay_over(
      table("[implementer]\nargs = \"--global\"\n"),
      table("[implementer]\n"),
    );

    assert_eq!(
      laid.to_string(),
      table("[implementer]\nargs = \"--global\"\n").to_string()
    );
  }

  #[test]
  fn should_take_the_layers_args_when_it_names_both_agent_and_args() {
    let laid = lay_over(
      table("[implementer]\nargs = \"--global\"\n"),
      table("[implementer]\nagent = \"codex\"\nargs = \"--project\"\n"),
    );

    assert_eq!(
      laid.to_string(),
      table("[implementer]\nagent = \"codex\"\nargs = \"--project\"\n").to_string()
    );
  }

  #[test]
  fn should_lay_a_role_over_nothing_when_the_table_lacks_it() {
    let laid = lay_over(table(""), table("[implementer]\nagent = \"codex\"\n"));

    assert_eq!(
      laid.to_string(),
      table("[implementer]\nagent = \"codex\"\n").to_string()
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
