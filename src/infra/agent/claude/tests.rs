use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

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

mod context_size {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&format!("{}\n{}\n", usage_line(10), usage_line(40)));

    assert_eq!(Claude.context_size(transcript.path()), 40);
  }

  #[test]
  fn should_read_zero_when_no_turn_reports_usage() {
    let transcript = Transcript::containing(r#"{"type":"user","message":{"content":"hi"}}"#);

    assert_eq!(Claude.context_size(transcript.path()), 0);
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
      10
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
      40
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

mod commits_in_transcript {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing(&assistant_line("[chainsaw 0123abc] fix: thing"));

    assert_eq!(
      Claude.commits_in_transcript(transcript.path(), 0),
      vec!["0123abc"]
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
      Claude.commits_in_transcript(transcript.path(), old.len() as u64 + 1),
      vec!["4567def"]
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
