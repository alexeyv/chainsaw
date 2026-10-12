use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::*;
use crate::domain::ContextSize;
use crate::domain::PromptEcho;

const DISALLOWED: &str = "WebSearch,WebFetch,NotebookEdit,Task,Agent,AskUserQuestion,EnterPlanMode,ExitPlanMode,TaskOutput";

/// A transcript file that is removed when dropped.
struct Transcript(PathBuf);

impl Transcript {
  fn containing(text: &str) -> Self {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
      "chainsaw-claude-transcript-{}-{}.jsonl",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
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

fn assistant_line(text: &str) -> String {
  format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#)
}

fn usage_line(input: u64) -> String {
  format!(r#"{{"type":"assistant","message":{{"usage":{{"input_tokens":{input}}}}}}}"#)
}

mod program {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(Claude.program(), "claude");
  }
}

mod default_args {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      Claude.default_args(SessionKind::Implementer),
      format!(
        "--model opus --effort high --disable-slash-commands --strict-mcp-config --no-chrome --disallowedTools {DISALLOWED}"
      )
    );
  }

  #[test]
  fn should_keep_slash_commands_for_the_commentator() {
    assert_eq!(
      Claude.default_args(SessionKind::Commentator),
      format!(
        "--model opus --effort high --strict-mcp-config --no-chrome --disallowedTools {DISALLOWED}"
      )
    );
  }
}

mod session_id_args {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      Claude.session_id_args("abc-123"),
      Some(vec!["--session-id".to_owned(), "abc-123".to_owned()])
    );
  }
}

mod context_size {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&format!("{}\n{}\n", usage_line(10), usage_line(40)));

    assert_eq!(
      Claude.context_size(transcript.path()),
      ContextSize::tokens(40)
    );
  }

  #[test]
  fn should_read_zero_when_no_turn_reports_usage() {
    let transcript = Transcript::containing(r#"{"type":"user","message":{"content":"hi"}}"#);

    assert_eq!(
      Claude.context_size(transcript.path()),
      ContextSize::tokens(0)
    );
  }
}

mod context_before {
  use super::*;

  #[test]
  fn should_work() {
    let first = usage_line(10);
    let transcript = Transcript::containing(&format!("{first}\n{}\n", usage_line(40)));

    assert_eq!(
      Claude.context_before(transcript.path(), first.len() as u64 + 1),
      ContextSize::tokens(10)
    );
  }
}

mod context_peak {
  use super::*;

  #[test]
  fn should_work() {
    let first = usage_line(90);
    let transcript = Transcript::containing(&format!(
      "{first}\n{}\n{}\n",
      usage_line(40),
      usage_line(20)
    ));

    assert_eq!(
      Claude.context_peak(transcript.path(), first.len() as u64 + 1, None),
      ContextSize::tokens(40)
    );
  }
}

mod prompt_state {
  use super::*;

  fn state_in(entries: &str, prompt: &str) -> PromptState {
    let transcript = Transcript::containing(entries);
    Claude.prompt_state(transcript.path(), 0, prompt)
  }

  #[test]
  fn should_work() {
    let transcript = r#"{"type":"user","message":{"content":"deliver this prompt"}}"#;

    assert_eq!(state_in(transcript, "deliver this"), PromptState::Started);
  }

  #[test]
  fn should_report_a_matching_enqueue() {
    let transcript =
      r#"{"type":"queue-operation","operation":"enqueue","content":"deliver this prompt"}"#;

    assert_eq!(state_in(transcript, "deliver this"), PromptState::Queued);
  }

  #[test]
  fn should_report_unseen_when_no_entry_matches() {
    let transcript = r#"{"type":"assistant","message":{"content":"deliver this prompt"}}"#;

    assert_eq!(state_in(transcript, "deliver this"), PromptState::Unseen);
  }

  #[test]
  fn should_ignore_an_enqueue_for_a_different_prompt() {
    let transcript =
      r#"{"type":"queue-operation","operation":"enqueue","content":"something else"}"#;

    assert_eq!(state_in(transcript, "deliver this"), PromptState::Unseen);
  }
}

mod prompt_echo {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(Claude.prompt_echo(), PromptEcho::OnTake);
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
      Claude.latest_assistant_text(transcript.path()).as_deref(),
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
      Claude.latest_assistant_text(transcript.path()).as_deref(),
      Some("said")
    );
  }
}

mod output_mentions {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&assistant_line("reviewed abc1234 and found nothing"));

    assert!(Claude.output_mentions(transcript.path(), "abc1234"));
  }

  #[test]
  fn should_ignore_mentions_by_the_user() {
    let transcript =
      Transcript::containing(r#"{"type":"user","message":{"content":"review abc1234"}}"#);

    assert!(!Claude.output_mentions(transcript.path(), "abc1234"));
  }
}

mod commit_candidates {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&assistant_line("[chainsaw 0123abc] fix: thing"));

    assert_eq!(
      Claude.commit_candidates(transcript.path(), 0, "head123"),
      vec!["0123abc".to_owned()]
    );
  }

  #[test]
  fn should_skip_commits_before_the_offset() {
    let old = assistant_line("[main 0123abc] old");
    let transcript = Transcript::containing(&format!(
      "{old}\n{}\n",
      assistant_line("[main 4567def] new")
    ));

    assert_eq!(
      Claude.commit_candidates(transcript.path(), old.len() as u64 + 1, "head123"),
      vec!["4567def".to_owned()]
    );
  }
}

mod transcript_of {
  use super::*;

  const SESSION: &str = "0b5c2e6a-1d3f-4a8b-9c7e-2f1a3b4c5d6e";
  const RUN_DIR: &str = "/Users/alex/my src/run_1";

  /// A projects directory that is removed when dropped.
  struct ProjectsDir(PathBuf);

  impl ProjectsDir {
    /// Holds the transcript of `SESSION` in a folder named `folder`.
    fn holding_session_in(folder: &str) -> Self {
      static NEXT: AtomicU64 = AtomicU64::new(0);
      let path = std::env::temp_dir().join(format!(
        "chainsaw-claude-projects-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
      ));
      let projects = Self(path);
      projects.hold_session_in(folder);
      projects
    }

    /// Also holds the transcript of `SESSION` in a folder named `folder`.
    fn hold_session_in(&self, folder: &str) {
      fs::create_dir_all(self.0.join(folder)).unwrap();
      fs::write(self.0.join(folder).join(format!("{SESSION}.jsonl")), "").unwrap();
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

  #[test]
  fn should_work() {
    let folder = transcripts_dir_name(Path::new(RUN_DIR));
    let projects = ProjectsDir::holding_session_in(&folder);

    assert_eq!(
      transcript_of(projects.path(), Path::new(RUN_DIR), SESSION),
      Some(
        projects
          .path()
          .join(&folder)
          .join(format!("{SESSION}.jsonl"))
      )
    );
  }

  #[test]
  fn should_find_the_session_whatever_its_folder_is_named() {
    let projects = ProjectsDir::holding_session_in("named-by-some-other-rule");

    assert_eq!(
      transcript_of(projects.path(), Path::new(RUN_DIR), SESSION),
      Some(
        projects
          .path()
          .join("named-by-some-other-rule")
          .join(format!("{SESSION}.jsonl"))
      )
    );
  }

  #[test]
  fn should_find_nothing_for_another_session() {
    let projects = ProjectsDir::holding_session_in("-run");

    assert_eq!(
      transcript_of(projects.path(), Path::new(RUN_DIR), "another-session"),
      None
    );
  }

  #[test]
  fn should_prefer_the_run_directorys_folder_when_another_holds_the_session_too() {
    let folder = transcripts_dir_name(Path::new(RUN_DIR));
    // Sorts before the run directory's folder, so a scan in name order
    // comes to it first.
    let projects = ProjectsDir::holding_session_in("-A-resumed-elsewhere");
    projects.hold_session_in(&folder);

    assert_eq!(
      transcript_of(projects.path(), Path::new(RUN_DIR), SESSION),
      Some(
        projects
          .path()
          .join(&folder)
          .join(format!("{SESSION}.jsonl"))
      )
    );
  }
}

mod newest_transcript_in {
  use super::*;

  /// A transcripts directory that is removed when dropped.
  struct TranscriptsDir(PathBuf);

  impl TranscriptsDir {
    fn holding(sessions: &[&str]) -> Self {
      static NEXT: AtomicU64 = AtomicU64::new(0);
      let path = std::env::temp_dir().join(format!(
        "chainsaw-claude-transcripts-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
      ));
      fs::create_dir_all(&path).unwrap();
      for session in sessions {
        fs::write(path.join(format!("{session}.jsonl")), "").unwrap();
      }
      Self(path)
    }

    fn path(&self) -> &Path {
      &self.0
    }
  }

  impl Drop for TranscriptsDir {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  fn a_minute_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(60)
  }

  #[test]
  fn should_work() {
    let dir = TranscriptsDir::holding(&["older"]);
    let older = dir.path().join("older.jsonl");
    fs::File::options()
      .write(true)
      .open(&older)
      .unwrap()
      .set_modified(a_minute_ago())
      .unwrap();
    fs::write(dir.path().join("newer.jsonl"), "").unwrap();

    assert_eq!(
      newest_transcript_in(dir.path(), a_minute_ago()),
      Some("newer".to_owned())
    );
  }

  #[test]
  fn should_find_nothing_when_every_transcript_predates_since() {
    let dir = TranscriptsDir::holding(&["stale"]);

    assert_eq!(
      newest_transcript_in(dir.path(), SystemTime::now() + Duration::from_secs(60)),
      None
    );
  }

  #[test]
  fn should_ignore_what_is_not_a_transcript() {
    let dir = TranscriptsDir::holding(&[]);
    fs::write(dir.path().join("chainsaw-supervisor.db"), "").unwrap();

    assert_eq!(newest_transcript_in(dir.path(), a_minute_ago()), None);
  }

  #[test]
  fn should_find_nothing_when_the_directory_does_not_exist() {
    assert_eq!(
      newest_transcript_in(Path::new("/nonexistent/transcripts"), a_minute_ago()),
      None
    );
  }
}

mod transcripts_dir_name {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      transcripts_dir_name(Path::new("/Users/alex/src/run")),
      "-Users-alex-src-run"
    );
  }

  #[test]
  fn should_dash_underscores_and_spaces() {
    assert_eq!(
      transcripts_dir_name(Path::new("/Users/alex/my src/run_1")),
      "-Users-alex-my-src-run-1"
    );
  }

  #[test]
  fn should_dash_a_windows_drive_and_backslashes() {
    assert_eq!(transcripts_dir_name(Path::new(r"C:\x\y_z")), "C--x-y-z");
  }

  #[test]
  fn should_dash_dots_when_the_run_directory_is_dotted() {
    assert_eq!(
      transcripts_dir_name(Path::new("/Users/alex/src/ui.wt/run")),
      "-Users-alex-src-ui-wt-run"
    );
  }

  #[test]
  fn should_dash_a_leading_dot_directory() {
    assert_eq!(transcripts_dir_name(Path::new("/x/.bare")), "-x--bare");
  }
}

mod usage_of_line {
  use super::*;

  #[test]
  fn should_work() {
    let line = r#"{"type":"assistant","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":5}}}"#;

    assert_eq!(usage_of_line(line), Some(10));
  }

  #[test]
  fn should_ignore_sidechain_usage() {
    let line = r#"{"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":99}}}"#;

    assert_eq!(usage_of_line(line), None);
  }
}
