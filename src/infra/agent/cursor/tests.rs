use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::*;
use crate::domain::ContextSize;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn scratch_path(kind: &str, extension: &str) -> PathBuf {
  std::env::temp_dir().join(format!(
    "chainsaw-cursor-{kind}-{}-{}{extension}",
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

/// A projects directory that is removed when dropped.
struct ProjectsDir(PathBuf);

impl ProjectsDir {
  fn empty() -> Self {
    let path = scratch_path("projects", "");
    fs::create_dir_all(&path).unwrap();
    Self(path)
  }

  fn holding(transcript: &str) -> Self {
    let dir = Self::empty();
    let path = dir.path().join(transcript);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "").unwrap();
    dir
  }

  fn path(&self) -> &Path {
    &self.0
  }
}

impl Drop for ProjectsDir {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

const SESSION: &str = "5b4c1a9e-2f3d-4e6a-8b7c-9d0e1f2a3b4c";
const RUN_DIR: &str = "/Users/alex/src/run";
const TRANSCRIPT: &str = "Users-alex-src-run/agent-transcripts/5b4c1a9e-2f3d-4e6a-8b7c-9d0e1f2a3b4c/5b4c1a9e-2f3d-4e6a-8b7c-9d0e1f2a3b4c.jsonl";

fn user_line(text: &str) -> String {
  format!(r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#)
}

/// The prompt as Cursor writes it: behind a timestamp, inside `user_query`.
fn prompt_line(prompt: &str) -> String {
  user_line(&format!(
    "<timestamp>Saturday, September 26, 2026 10:00 AM</timestamp>\\n<user_query>\\n{prompt}\\n</user_query>"
  ))
}

fn assistant_line(text: &str) -> String {
  format!(r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#)
}

fn tool_use_line(command: &str) -> String {
  format!(
    r#"{{"role":"assistant","message":{{"content":[{{"type":"tool_use","name":"shell","input":{{"command":"{command}"}}}}]}}}}"#
  )
}

const TURN_ENDED: &str = r#"{"type":"turn_ended","status":"success"}"#;

mod program {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(Cursor.program(), "cursor-agent");
  }
}

mod default_args {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      Cursor.default_args(SessionKind::Implementer),
      "--trust --force ."
    );
  }

  #[test]
  fn should_be_the_same_for_the_commentator() {
    assert_eq!(
      Cursor.default_args(SessionKind::Commentator),
      Cursor.default_args(SessionKind::Implementer)
    );
  }
}

mod session_id_args {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(Cursor.session_id_args("abc-123"), None);
  }
}

mod compact_prompt {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(Cursor.compact_prompt(), "/summarize");
  }
}

mod context_size {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&format!(
      "{}\n{}\n{TURN_ENDED}\n",
      prompt_line("do it"),
      assistant_line("done")
    ));

    assert_eq!(Cursor.context_size(transcript.path()), ContextSize::UNKNOWN);
  }
}

mod context_before {
  use super::*;

  #[test]
  fn should_work() {
    let first = prompt_line("do it");
    let transcript = Transcript::containing(&format!("{first}\n{}\n", assistant_line("done")));

    assert_eq!(
      Cursor.context_before(transcript.path(), first.len() as u64 + 1),
      ContextSize::UNKNOWN
    );
  }
}

mod context_peak {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&format!(
      "{}\n{}\n",
      prompt_line("do it"),
      assistant_line("done")
    ));

    assert_eq!(
      Cursor.context_peak(transcript.path(), 0, None),
      ContextSize::UNKNOWN
    );
  }
}

mod prompt_state {
  use super::*;

  fn state_in(entries: &str, prompt: &str) -> PromptState {
    let transcript = Transcript::containing(entries);
    Cursor.prompt_state(transcript.path(), 0, prompt)
  }

  #[test]
  fn should_work() {
    assert_eq!(
      state_in(&prompt_line("deliver this prompt"), "deliver this"),
      PromptState::Started
    );
  }

  #[test]
  fn should_report_unseen_when_no_entry_holds_the_prompt() {
    assert_eq!(
      state_in(&prompt_line("something else"), "deliver this"),
      PromptState::Unseen
    );
  }

  #[test]
  fn should_report_unseen_when_only_a_reply_holds_the_text() {
    assert_eq!(
      state_in(&assistant_line("deliver this prompt"), "deliver this"),
      PromptState::Unseen
    );
  }

  #[test]
  fn should_skip_entries_before_the_offset() {
    let old = prompt_line("deliver this prompt");
    let transcript = Transcript::containing(&format!("{old}\n{}\n", assistant_line("done")));

    assert_eq!(
      Cursor.prompt_state(transcript.path(), old.len() as u64 + 1, "deliver this"),
      PromptState::Unseen
    );
  }

  #[test]
  fn should_see_the_prompt_when_the_new_turn_dropped_the_turn_ended_line_before_it() {
    let before = format!("{}\n{TURN_ENDED}\n", assistant_line("done"));
    let offset = before.len() as u64;
    let after = format!(
      "{}\n{}\n",
      assistant_line("done"),
      prompt_line("deliver this prompt")
    );
    let transcript = Transcript::containing(&after);

    assert_eq!(
      Cursor.prompt_state(transcript.path(), offset, "deliver this"),
      PromptState::Started
    );
  }

  #[test]
  fn should_tolerate_an_offset_past_the_end() {
    let transcript = Transcript::containing(&format!("{}\n", prompt_line("deliver this prompt")));

    assert_eq!(
      Cursor.prompt_state(transcript.path(), 10_000, "deliver this"),
      PromptState::Unseen
    );
  }
}

mod prompt_attempts {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(Cursor.prompt_attempts(), 1);
  }
}

mod latest_assistant_text {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&format!(
      "{}\n{}\n{}\n{TURN_ENDED}\n",
      prompt_line("do it"),
      assistant_line("first"),
      assistant_line("second")
    ));

    assert_eq!(
      Cursor.latest_assistant_text(transcript.path()).as_deref(),
      Some("second")
    );
  }

  #[test]
  fn should_ignore_a_tool_use_after_the_last_text() {
    let transcript = Transcript::containing(&format!(
      "{}\n{}\n",
      assistant_line("said"),
      tool_use_line("git status")
    ));

    assert_eq!(
      Cursor.latest_assistant_text(transcript.path()).as_deref(),
      Some("said")
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
      Cursor.latest_assistant_text(transcript.path()).as_deref(),
      Some("said")
    );
  }

  #[test]
  fn should_report_nothing_when_only_the_user_spoke() {
    let transcript = Transcript::containing(&prompt_line("hello"));

    assert_eq!(Cursor.latest_assistant_text(transcript.path()), None);
  }
}

mod output_mentions {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&assistant_line("reviewed abc1234 and found nothing"));

    assert!(Cursor.output_mentions(transcript.path(), "abc1234"));
  }

  #[test]
  fn should_count_a_tool_use_input() {
    let transcript = Transcript::containing(&tool_use_line("git show abc1234"));

    assert!(Cursor.output_mentions(transcript.path(), "abc1234"));
  }

  #[test]
  fn should_ignore_mentions_by_the_user() {
    let transcript = Transcript::containing(&prompt_line("review abc1234"));

    assert!(!Cursor.output_mentions(transcript.path(), "abc1234"));
  }
}

mod commit_candidates {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&assistant_line("[chainsaw 0123abc] fix: thing"));

    assert_eq!(
      Cursor.commit_candidates(transcript.path(), 0, "head123"),
      vec!["head123".to_owned()]
    );
  }
}

mod newest_session_in {
  use super::*;

  fn a_minute_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(60)
  }

  fn transcript_of(session: &str) -> String {
    format!("Users-alex-src-run/agent-transcripts/{session}/{session}.jsonl")
  }

  fn age(path: &Path) {
    fs::File::open(path)
      .unwrap()
      .set_modified(a_minute_ago() - Duration::from_secs(60))
      .unwrap();
  }

  #[test]
  fn should_work() {
    let projects = ProjectsDir::holding(&transcript_of("older"));
    age(&projects.path().join(transcript_of("older")));
    let newer = projects.path().join(transcript_of("newer"));
    fs::create_dir_all(newer.parent().unwrap()).unwrap();
    fs::write(&newer, "").unwrap();

    assert_eq!(
      newest_session_in(projects.path(), Path::new(RUN_DIR), a_minute_ago()),
      Some("newer".to_owned())
    );
  }

  #[test]
  fn should_ignore_a_session_of_another_project() {
    let elsewhere = format!("Users-alex-src/agent-transcripts/{SESSION}/{SESSION}.jsonl");
    let projects = ProjectsDir::holding(&elsewhere);

    assert_eq!(
      newest_session_in(projects.path(), Path::new(RUN_DIR), a_minute_ago()),
      None
    );
  }

  #[test]
  fn should_ignore_what_is_not_a_sessions_transcript() {
    let projects = ProjectsDir::holding("Users-alex-src-run/agent-transcripts/notes/scratch.jsonl");

    assert_eq!(
      newest_session_in(projects.path(), Path::new(RUN_DIR), a_minute_ago()),
      None
    );
  }

  #[test]
  fn should_find_nothing_when_every_transcript_predates_since() {
    let projects = ProjectsDir::holding(TRANSCRIPT);

    assert_eq!(
      newest_session_in(
        projects.path(),
        Path::new(RUN_DIR),
        SystemTime::now() + Duration::from_secs(60)
      ),
      None
    );
  }

  #[test]
  fn should_find_nothing_when_the_projects_directory_is_missing() {
    let projects = ProjectsDir::empty();
    let missing = projects.path().join("missing");

    assert_eq!(
      newest_session_in(&missing, Path::new(RUN_DIR), a_minute_ago()),
      None
    );
  }
}

mod transcript_in {
  use super::*;

  #[test]
  fn should_work() {
    let projects = ProjectsDir::holding(TRANSCRIPT);

    assert_eq!(
      transcript_in(projects.path(), Path::new(RUN_DIR), SESSION),
      Some(projects.path().join(TRANSCRIPT))
    );
  }

  #[test]
  fn should_find_the_transcript_under_another_project_when_cursor_disagrees_about_the_run_dir() {
    let elsewhere = format!("Users-alex-src/agent-transcripts/{SESSION}/{SESSION}.jsonl");
    let projects = ProjectsDir::holding(&elsewhere);

    assert_eq!(
      transcript_in(projects.path(), Path::new(RUN_DIR), SESSION),
      Some(projects.path().join(elsewhere))
    );
  }

  #[test]
  fn should_report_nothing_when_no_transcript_names_the_session() {
    let projects = ProjectsDir::holding(TRANSCRIPT);

    assert_eq!(
      transcript_in(projects.path(), Path::new(RUN_DIR), "other-session"),
      None
    );
  }

  #[test]
  fn should_report_nothing_when_the_projects_directory_is_missing() {
    let projects = ProjectsDir::empty();
    let missing = projects.path().join("missing");

    assert_eq!(transcript_in(&missing, Path::new(RUN_DIR), SESSION), None);
  }
}

mod project_name {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(project_name(Path::new(RUN_DIR)), "Users-alex-src-run");
  }

  #[test]
  fn should_turn_every_run_of_other_characters_into_one_dash_and_trim_the_ends() {
    assert_eq!(project_name(Path::new("/a/b.c_d/")), "a-b-c-d");
  }

  #[test]
  fn should_drop_the_dot_of_a_hidden_directory() {
    assert_eq!(project_name(Path::new("/x/.bare")), "x-bare");
  }
}

mod transcript_named {
  use super::*;

  #[test]
  fn should_work() {
    let projects = ProjectsDir::holding(TRANSCRIPT);

    assert_eq!(
      transcript_named(projects.path(), &format!("{SESSION}.jsonl"), 3),
      Some(projects.path().join(TRANSCRIPT))
    );
  }

  #[test]
  fn should_stop_at_the_depth_given() {
    let projects = ProjectsDir::holding(TRANSCRIPT);

    assert_eq!(
      transcript_named(projects.path(), &format!("{SESSION}.jsonl"), 2),
      None
    );
  }

  #[test]
  fn should_report_nothing_when_the_directory_is_missing() {
    let projects = ProjectsDir::empty();
    let missing = projects.path().join("missing");

    assert_eq!(transcript_named(&missing, "any.jsonl", 3), None);
  }
}
