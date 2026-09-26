use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use std::collections::BTreeMap;

use super::{PromptLanding, format_growth, prompt_landed, transcript_growth, usage_of_line};

fn sizes(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
  pairs
    .iter()
    .map(|(name, size)| ((*name).to_owned(), *size))
    .collect()
}

mod transcript_growth {
  use super::*;

  #[test]
  fn should_work() {
    let before = sizes(&[("a", 10), ("b", 20)]);
    let after = sizes(&[("a", 15), ("b", 20)]);

    assert_eq!(
      format!("{:?}", transcript_growth(&before, &after)),
      r#"[("a", 5)]"#
    );
  }

  #[test]
  fn should_count_a_new_transcript_as_growth_from_zero() {
    let before = sizes(&[("a", 10)]);
    let after = sizes(&[("a", 10), ("b", 7)]);

    assert_eq!(
      format!("{:?}", transcript_growth(&before, &after)),
      r#"[("b", 7)]"#
    );
  }

  #[test]
  fn should_ignore_a_transcript_that_shrank_or_vanished() {
    let before = sizes(&[("a", 10), ("b", 20)]);
    let after = sizes(&[("a", 4)]);

    assert_eq!(format!("{:?}", transcript_growth(&before, &after)), "[]");
  }
}

mod format_growth {
  use super::*;

  #[test]
  fn should_work() {
    let growth = vec![("a".to_owned(), 5), ("b".to_owned(), 2)];

    assert_eq!(
      format_growth(&growth).as_deref(),
      Some("transcripts grew: a +5, b +2")
    );
  }

  #[test]
  fn should_be_silent_when_nothing_grew() {
    assert_eq!(format_growth(&[]), None);
  }
}

fn landing_in(transcript: &str, needle: &str) -> Option<PromptLanding> {
  static NEXT_TRANSCRIPT: AtomicU64 = AtomicU64::new(0);
  let path = std::env::temp_dir().join(format!(
    "chainsaw-prompt-landing-{}-{}.jsonl",
    std::process::id(),
    NEXT_TRANSCRIPT.fetch_add(1, Ordering::Relaxed)
  ));
  fs::write(&path, transcript).unwrap();
  let landing = prompt_landed(&path, 0, needle);
  let _ = fs::remove_file(path);
  landing
}

mod prompt_landed {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = r#"{"type":"user","message":{"content":"deliver this prompt"}}"#;

    assert_eq!(
      landing_in(transcript, "deliver this"),
      Some(PromptLanding::Landed)
    );
  }

  #[test]
  fn should_report_a_matching_enqueue() {
    let transcript =
      r#"{"type":"queue-operation","operation":"enqueue","content":"deliver this prompt"}"#;

    assert_eq!(
      landing_in(transcript, "deliver this"),
      Some(PromptLanding::Queued)
    );
  }

  #[test]
  fn should_report_neither_when_no_entry_matches() {
    let transcript = r#"{"type":"assistant","message":{"content":"deliver this prompt"}}"#;

    assert_eq!(landing_in(transcript, "deliver this"), None);
  }

  #[test]
  fn should_ignore_an_enqueue_for_a_different_prompt() {
    let transcript =
      r#"{"type":"queue-operation","operation":"enqueue","content":"something else"}"#;

    assert_eq!(landing_in(transcript, "deliver this"), None);
  }
}

#[test]
fn sums_context_tokens() {
  let line = r#"{"type":"assistant","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":5}}}"#;
  assert_eq!(usage_of_line(line), Some(10));
}

#[test]
fn ignores_sidechain_usage() {
  let line = r#"{"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":99}}}"#;
  assert_eq!(usage_of_line(line), None);
}
