use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::session_runtime::SessionKind;

mod parse {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(AgentCli::parse("claude").unwrap(), AgentCli::Claude);
    assert_eq!(AgentCli::parse("claude-code").unwrap(), AgentCli::Claude);
    assert_eq!(AgentCli::parse("cursor").unwrap(), AgentCli::Cursor);
    assert_eq!(AgentCli::parse("cursor-agent").unwrap(), AgentCli::Cursor);
    assert_eq!(AgentCli::parse("codex-cli").unwrap(), AgentCli::Codex);
  }

  #[test]
  fn should_fail_when_the_cli_is_unknown() {
    let error = AgentCli::parse("hermes").unwrap_err();
    assert_eq!(
      error.to_string(),
      r#"unknown agent cli "hermes"; expected claude, cursor, or codex"#
    );
  }
}

mod spec_parse {
  use super::*;

  #[test]
  fn should_work() {
    let spec = AgentSpec::parse(&serde_json::json!({
      "cli": "cursor",
      "model": "gpt-5",
      "args": ["--force"]
    }))
    .unwrap();

    assert_eq!(spec.cli(), AgentCli::Cursor);
    assert_eq!(spec.model(), Some("gpt-5"));
    assert_eq!(spec.args(), &["--force".to_owned()]);
  }

  #[test]
  fn should_default_claude_to_opus_when_the_model_is_omitted() {
    let spec = AgentSpec::parse(&serde_json::json!({"cli": "claude"})).unwrap();

    assert_eq!(spec, AgentSpec::claude_opus());
  }

  #[test]
  fn should_omit_the_model_when_cursor_does_not_name_one() {
    let spec = AgentSpec::parse(&serde_json::json!({"cli": "cursor"})).unwrap();

    assert_eq!(spec.model(), None);
  }

  #[test]
  fn should_fail_when_cli_is_missing() {
    let error = AgentSpec::parse(&serde_json::json!({"model": "opus"})).unwrap_err();
    assert_eq!(error.to_string(), "agent spec needs a cli");
  }

  #[test]
  fn should_fail_when_a_key_is_unknown() {
    let error = AgentSpec::parse(&serde_json::json!({"cli": "claude", "foo": 1})).unwrap_err();
    assert_eq!(error.to_string(), r#"unknown agent setting "foo""#);
  }
}

mod launch_flags {
  use super::*;

  #[test]
  fn should_work() {
    let spec = AgentSpec::claude_opus();

    assert_eq!(
      spec.launch_flags(SessionKind::Implementer),
      [
        "--model",
        "opus",
        "--effort",
        "high",
        "--disable-slash-commands",
        "--strict-mcp-config",
        "--no-chrome",
        "--disallowedTools",
        "WebSearch,WebFetch,NotebookEdit,Task,Agent,AskUserQuestion,EnterPlanMode,ExitPlanMode,TaskOutput",
      ]
    );
  }

  #[test]
  fn should_omit_disable_slash_commands_for_a_commentator() {
    let spec = AgentSpec::claude_opus();
    let flags = spec.launch_flags(SessionKind::Commentator);

    assert!(!flags.iter().any(|flag| flag == "--disable-slash-commands"));
    assert_eq!(flags[0], "--model");
    assert_eq!(flags[1], "opus");
  }

  #[test]
  fn should_trust_and_force_cursor() {
    let spec = AgentSpec::new(AgentCli::Cursor, Some("gpt-5".to_owned()), Vec::new()).unwrap();

    assert_eq!(
      spec.launch_flags(SessionKind::Implementer),
      ["--trust", "--force", "--model", "gpt-5"]
    );
  }

  #[test]
  fn should_append_extra_args_after_cursor_defaults() {
    let spec = AgentSpec::new(AgentCli::Cursor, None, vec!["--approve-mcps".to_owned()]).unwrap();

    assert_eq!(
      spec.launch_flags(SessionKind::Implementer),
      ["--trust", "--force", "--approve-mcps"]
    );
  }

  #[test]
  fn should_pass_no_flags_when_codex_has_no_model() {
    let spec = AgentSpec::new(AgentCli::Codex, None, Vec::new()).unwrap();

    assert!(spec.launch_flags(SessionKind::Implementer).is_empty());
  }
}

mod cursor_project_directory_name {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      cursor_project_directory_name(Path::new("/Users/a/src/app")),
      "Users-a-src-app"
    );
  }
}

mod find_named_jsonl {
  use super::*;

  struct Scratch(PathBuf);

  impl Scratch {
    fn new() -> Self {
      static COUNTER: AtomicU64 = AtomicU64::new(0);
      let path = std::env::temp_dir().join(format!(
        "chainsaw-agent-search-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
      ));
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }
  }

  impl Drop for Scratch {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  #[test]
  fn should_work() {
    let scratch = Scratch::new();
    let file = scratch
      .0
      .join("projects/app/agent-transcripts/sess-1/sess-1.jsonl");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "{}\n").unwrap();

    assert_eq!(
      find_named_jsonl(&scratch.0, "sess-1.jsonl", 4).as_deref(),
      Some(file.as_path())
    );
  }

  #[test]
  fn should_find_a_codex_rollout_by_session_id() {
    let scratch = Scratch::new();
    let file = scratch
      .0
      .join("sessions/2026/09/10/rollout-2026-09-10T01-00-00-sess-codex.jsonl");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "{}\n").unwrap();

    assert_eq!(
      find_jsonl_containing(&scratch.0, "sess-codex", 4).as_deref(),
      Some(file.as_path())
    );
  }
}
