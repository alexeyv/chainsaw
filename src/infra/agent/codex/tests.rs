use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn scratch_path(kind: &str, extension: &str) -> PathBuf {
  std::env::temp_dir().join(format!(
    "chainsaw-codex-{kind}-{}-{}{extension}",
    std::process::id(),
    NEXT.fetch_add(1, Ordering::Relaxed)
  ))
}

/// A transcript file that is removed when dropped.
struct Transcript(PathBuf);

impl Transcript {
  fn containing(text: &str) -> Self {
    let path = scratch_path("transcript", ".jsonl");
    fs::write(&path, text).unwrap();
    Self(path)
  }

  fn path(&self) -> &Path {
    &self.0
  }
}

impl Drop for Transcript {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
  }
}

/// A sessions directory that is removed when dropped.
struct SessionsDir(PathBuf);

impl SessionsDir {
  fn empty() -> Self {
    let path = scratch_path("sessions", "");
    fs::create_dir_all(&path).unwrap();
    Self(path)
  }

  fn holding(rollout: &str) -> Self {
    let dir = Self::empty();
    let path = dir.path().join(rollout);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "").unwrap();
    dir
  }

  fn path(&self) -> &Path {
    &self.0
  }
}

impl Drop for SessionsDir {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

const SESSION: &str = "01a0dc53-6097-7111-be72-8743101bf1d8";
const ROLLOUT: &str =
  "2026/09/26/rollout-2026-09-26T00-07-26-01a0dc53-6097-7111-be72-8743101bf1d8.jsonl";

fn user_line(text: &str) -> String {
  format!(
    r#"{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{text}"}}]}}}}"#
  )
}

fn assistant_line(text: &str) -> String {
  format!(
    r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{text}"}}]}}}}"#
  )
}

fn tool_call_line(command: &str) -> String {
  format!(
    r#"{{"type":"response_item","payload":{{"type":"custom_tool_call","name":"exec","input":"{command}"}}}}"#
  )
}

fn tool_output_line(text: &str) -> String {
  format!(
    r#"{{"type":"response_item","payload":{{"type":"function_call_output","output":"{text}"}}}}"#
  )
}

/// The usage Codex records after one response: `input` is the context that
/// response was handed, `cached` the part of it served from cache, and
/// `total` the thread's input so far.
fn usage_line(input: u64, cached: u64, total: u64) -> String {
  format!(
    r#"{{"type":"token_usage_record","payload":{{"usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":60}},"thread_token_usage":{{"input_tokens":{total},"cached_input_tokens":{cached}}}}}}}"#
  )
}

mod default_args {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      Codex.default_args(SessionKind::Implementer),
      "--dangerously-bypass-approvals-and-sandbox ."
    );
  }

  #[test]
  fn should_be_the_same_for_the_commentator() {
    assert_eq!(
      Codex.default_args(SessionKind::Commentator),
      Codex.default_args(SessionKind::Implementer)
    );
  }
}

mod context_size {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&format!(
      "{}\n{}\n",
      usage_line(10, 0, 10),
      usage_line(40, 8, 50)
    ));

    assert_eq!(Codex.context_size(transcript.path()), 40);
  }

  #[test]
  fn should_read_the_last_response_input_not_the_thread_total() {
    let transcript = Transcript::containing(&format!(
      "{}\n{}\n",
      usage_line(19_519, 7_808, 19_519),
      usage_line(65_537, 60_000, 2_052_395)
    ));

    assert_eq!(Codex.context_size(transcript.path()), 65_537);
  }

  #[test]
  fn should_read_zero_when_no_response_reports_usage() {
    let transcript = Transcript::containing(&user_line("hi"));

    assert_eq!(Codex.context_size(transcript.path()), 0);
  }
}

mod context_before {
  use super::*;

  #[test]
  fn should_work() {
    let first = usage_line(10, 0, 10);
    let transcript = Transcript::containing(&format!("{first}\n{}\n", usage_line(40, 0, 50)));

    assert_eq!(
      Codex.context_before(transcript.path(), first.len() as u64 + 1),
      10
    );
  }
}

mod context_peak {
  use super::*;

  #[test]
  fn should_work() {
    let first = usage_line(90, 0, 90);
    let transcript = Transcript::containing(&format!(
      "{first}\n{}\n{}\n",
      usage_line(40, 0, 130),
      usage_line(20, 0, 150)
    ));

    assert_eq!(
      Codex.context_peak(transcript.path(), first.len() as u64 + 1, None),
      40
    );
  }
}

mod prompt_state {
  use super::*;

  fn state_in(entries: &str, prompt: &str) -> PromptState {
    let transcript = Transcript::containing(entries);
    Codex.prompt_state(transcript.path(), 0, prompt)
  }

  #[test]
  fn should_work() {
    let transcript = format!(
      "{}\n{}\n{}\n",
      user_line("# AGENTS.md instructions for /Users/alex/run"),
      user_line("<environment_context><cwd>/Users/alex/run</cwd></environment_context>"),
      user_line("deliver this prompt")
    );

    assert_eq!(state_in(&transcript, "deliver this"), PromptState::Started);
  }

  #[test]
  fn should_report_unseen_when_only_codex_own_user_messages_are_present() {
    let transcript = format!(
      "{}\n{}\n",
      user_line("# AGENTS.md instructions for /Users/alex/run"),
      user_line("<environment_context><cwd>/Users/alex/run</cwd></environment_context>")
    );

    assert_eq!(state_in(&transcript, "deliver this"), PromptState::Unseen);
  }

  #[test]
  fn should_report_unseen_when_only_a_reply_holds_the_text() {
    assert_eq!(
      state_in(&assistant_line("deliver this prompt"), "deliver this"),
      PromptState::Unseen
    );
  }
}

mod latest_assistant_text {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&format!(
      "{}\n{}\n",
      assistant_line("first"),
      assistant_line("second")
    ));

    assert_eq!(
      Codex.latest_assistant_text(transcript.path()).as_deref(),
      Some("second")
    );
  }

  #[test]
  fn should_skip_blank_text_blocks() {
    let transcript = Transcript::containing(&format!(
      "{}\n{}\n",
      assistant_line("said"),
      assistant_line("  ")
    ));

    assert_eq!(
      Codex.latest_assistant_text(transcript.path()).as_deref(),
      Some("said")
    );
  }

  #[test]
  fn should_report_nothing_when_only_the_user_spoke() {
    let transcript = Transcript::containing(&user_line("hello"));

    assert_eq!(Codex.latest_assistant_text(transcript.path()), None);
  }
}

mod output_mentions {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&assistant_line("reviewed abc1234 and found nothing"));

    assert!(Codex.output_mentions(transcript.path(), "abc1234"));
  }

  #[test]
  fn should_count_a_tool_call() {
    let transcript = Transcript::containing(&tool_call_line("git show abc1234"));

    assert!(Codex.output_mentions(transcript.path(), "abc1234"));
  }

  #[test]
  fn should_ignore_a_tool_output() {
    let transcript = Transcript::containing(&tool_output_line("abc1234 fix: thing"));

    assert!(!Codex.output_mentions(transcript.path(), "abc1234"));
  }

  #[test]
  fn should_ignore_mentions_by_the_user() {
    let transcript = Transcript::containing(&user_line("review abc1234"));

    assert!(!Codex.output_mentions(transcript.path(), "abc1234"));
  }
}

mod commits_in_transcript {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&tool_output_line("[chainsaw 0123abc] fix: thing"));

    assert_eq!(
      Codex.commits_in_transcript(transcript.path(), 0),
      vec!["0123abc"]
    );
  }

  #[test]
  fn should_skip_commits_before_the_offset() {
    let old = tool_output_line("[main 0123abc] old");
    let transcript = Transcript::containing(&format!(
      "{old}\n{}\n",
      tool_output_line("[main 4567def] new")
    ));

    assert_eq!(
      Codex.commits_in_transcript(transcript.path(), old.len() as u64 + 1),
      vec!["4567def"]
    );
  }
}

mod rollout_of {
  use super::*;

  #[test]
  fn should_work() {
    let sessions = SessionsDir::holding(ROLLOUT);

    assert_eq!(
      rollout_of(sessions.path(), SESSION, 4),
      Some(sessions.path().join(ROLLOUT))
    );
  }

  #[test]
  fn should_report_nothing_when_no_rollout_names_the_session() {
    let sessions = SessionsDir::holding(ROLLOUT);

    assert_eq!(rollout_of(sessions.path(), "other-session", 4), None);
  }

  #[test]
  fn should_report_nothing_when_the_sessions_directory_is_missing() {
    let sessions = SessionsDir::empty();
    let missing = sessions.path().join("missing");

    assert_eq!(rollout_of(&missing, SESSION, 4), None);
  }

  #[test]
  fn should_stop_at_the_depth_given() {
    let sessions = SessionsDir::holding(ROLLOUT);

    assert_eq!(rollout_of(sessions.path(), SESSION, 2), None);
  }
}

mod usage_of_line {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      usage_of_line(&usage_line(19_519, 7_808, 19_519)),
      Some(19_519)
    );
  }

  #[test]
  fn should_ignore_a_token_count_event() {
    let line = r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2052395},"last_token_usage":{"input_tokens":65537}}}}"#;

    assert_eq!(usage_of_line(line), None);
  }
}
