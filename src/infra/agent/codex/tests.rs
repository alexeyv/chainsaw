use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::*;
use crate::domain::ContextSize;
use crate::domain::PromptEcho;

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
    dir.write(rollout, "");
    dir
  }

  /// Puts a rollout with these contents below the directory.
  fn write(&self, rollout: &str, text: &str) -> PathBuf {
    let path = self.path().join(rollout);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
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

mod program {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(Codex.program(), "codex");
  }
}

mod default_args {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      Codex.default_args(SessionKind::Implementer),
      "--dangerously-bypass-approvals-and-sandbox"
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

mod session_id_args {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(Codex.session_id_args("abc-123"), None);
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

    assert_eq!(
      Codex.context_size(transcript.path()),
      ContextSize::tokens(40)
    );
  }

  #[test]
  fn should_read_the_last_response_input_not_the_thread_total() {
    let transcript = Transcript::containing(&format!(
      "{}\n{}\n",
      usage_line(19_519, 7_808, 19_519),
      usage_line(65_537, 60_000, 2_052_395)
    ));

    assert_eq!(
      Codex.context_size(transcript.path()),
      ContextSize::tokens(65_537)
    );
  }

  #[test]
  fn should_read_zero_when_no_response_reports_usage() {
    let transcript = Transcript::containing(&user_line("hi"));

    assert_eq!(
      Codex.context_size(transcript.path()),
      ContextSize::tokens(0)
    );
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
      ContextSize::tokens(10)
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
      ContextSize::tokens(40)
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

mod prompt_echo {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(Codex.prompt_echo(), PromptEcho::OnTake);
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

mod commit_candidates {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&tool_output_line("[chainsaw 0123abc] fix: thing"));

    assert_eq!(
      Codex.commit_candidates(transcript.path(), 0, "head123"),
      vec!["0123abc".to_owned()]
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
      Codex.commit_candidates(transcript.path(), old.len() as u64 + 1, "head123"),
      vec!["4567def".to_owned()]
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

/// The opening line Codex writes for a session that started in `cwd`.
fn session_meta_line(id: &str, cwd: &str) -> String {
  format!(
    r#"{{"timestamp":"2026-09-26T00:07:26.000Z","type":"session_meta","payload":{{"id":"{id}","cwd":"{cwd}","cli_version":"0.157.1"}}}}"#
  )
}

fn a_minute_ago() -> SystemTime {
  SystemTime::now() - Duration::from_secs(60)
}

mod newest_rollout_in {
  use super::*;

  #[test]
  fn should_work() {
    let dir = SessionsDir::empty();
    let older = dir.write(
      "2026/09/26/rollout-2026-09-26T00-07-26-older.jsonl",
      &session_meta_line("older", "/tmp/run"),
    );
    fs::File::open(&older)
      .unwrap()
      .set_modified(a_minute_ago())
      .unwrap();
    dir.write(
      "2026/09/26/rollout-2026-09-26T00-08-26-newer.jsonl",
      &session_meta_line("newer", "/tmp/run"),
    );

    assert_eq!(
      newest_rollout_in(dir.path(), Path::new("/tmp/run"), a_minute_ago(), 4),
      Some("newer".to_owned())
    );
  }

  #[test]
  fn should_skip_a_newer_session_from_another_directory() {
    let dir = SessionsDir::empty();
    let ours = dir.write(
      "2026/09/26/rollout-2026-09-26T00-07-26-ours.jsonl",
      &session_meta_line("ours", "/tmp/run"),
    );
    fs::File::open(&ours)
      .unwrap()
      .set_modified(SystemTime::now() - Duration::from_secs(30))
      .unwrap();
    dir.write(
      "2026/09/26/rollout-2026-09-26T00-08-26-theirs.jsonl",
      &session_meta_line("theirs", "/tmp/elsewhere"),
    );

    assert_eq!(
      newest_rollout_in(dir.path(), Path::new("/tmp/run"), a_minute_ago(), 4),
      Some("ours".to_owned())
    );
  }

  #[test]
  fn should_find_nothing_when_every_rollout_predates_since() {
    let dir = SessionsDir::empty();
    dir.write(
      "2026/09/26/rollout-2026-09-26T00-07-26-stale.jsonl",
      &session_meta_line("stale", "/tmp/run"),
    );

    assert_eq!(
      newest_rollout_in(
        dir.path(),
        Path::new("/tmp/run"),
        SystemTime::now() + Duration::from_secs(60),
        4
      ),
      None
    );
  }

  #[test]
  fn should_find_nothing_when_the_directory_does_not_exist() {
    assert_eq!(
      newest_rollout_in(
        Path::new("/nonexistent/sessions"),
        Path::new("/tmp/run"),
        a_minute_ago(),
        4
      ),
      None
    );
  }
}

mod session_of_rollout_in {
  use super::*;

  #[test]
  fn should_work() {
    let dir = SessionsDir::empty();
    let rollout = dir.write(
      "rollout.jsonl",
      &format!(
        "{}\n{}\n",
        session_meta_line(SESSION, "/tmp/run"),
        user_line("hi")
      ),
    );

    assert_eq!(
      session_of_rollout_in(&rollout, Path::new("/tmp/run")),
      Some(SESSION.to_owned())
    );
  }

  #[test]
  fn should_resolve_the_recorded_directory_before_comparing() {
    let dir = SessionsDir::empty();
    let real = dir.path().join("real");
    fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
    let rollout = dir.write(
      "rollout.jsonl",
      &session_meta_line(SESSION, dir.path().join("link").to_str().unwrap()),
    );

    assert_eq!(
      session_of_rollout_in(&rollout, &real.canonicalize().unwrap()),
      Some(SESSION.to_owned())
    );
  }

  #[test]
  fn should_find_nothing_when_the_session_ran_elsewhere() {
    let dir = SessionsDir::empty();
    let rollout = dir.write(
      "rollout.jsonl",
      &session_meta_line(SESSION, "/tmp/elsewhere"),
    );

    assert_eq!(session_of_rollout_in(&rollout, Path::new("/tmp/run")), None);
  }

  #[test]
  fn should_find_nothing_when_the_rollout_does_not_open_with_its_session() {
    let dir = SessionsDir::empty();
    let rollout = dir.write("rollout.jsonl", &user_line("hi"));

    assert_eq!(session_of_rollout_in(&rollout, Path::new("/tmp/run")), None);
  }

  #[test]
  fn should_find_nothing_in_an_empty_rollout() {
    let dir = SessionsDir::holding("rollout.jsonl");

    assert_eq!(
      session_of_rollout_in(&dir.path().join("rollout.jsonl"), Path::new("/tmp/run")),
      None
    );
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
