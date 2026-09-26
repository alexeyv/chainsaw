use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// A transcript file that is removed when dropped.
struct Transcript(PathBuf);

impl Transcript {
  fn containing(text: &str) -> Self {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
      "chainsaw-monitor-transcript-{}-{}.jsonl",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, text).unwrap();
    Self(path)
  }

  fn missing() -> Self {
    let transcript = Self::containing("");
    fs::remove_file(&transcript.0).unwrap();
    transcript
  }

  fn append(&self, text: &str) {
    let mut content = fs::read_to_string(&self.0).unwrap_or_default();
    content.push_str(text);
    fs::write(&self.0, content).unwrap();
  }

  fn remove(&self) {
    let _ = fs::remove_file(&self.0);
  }

  fn path(&self) -> &Path {
    &self.0
  }

  fn named(&self, name: &str) -> (String, PathBuf) {
    (name.to_owned(), self.0.clone())
  }
}

impl Drop for Transcript {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
  }
}

mod transcript_size {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::containing("0123456789");

    assert_eq!(transcript_size(Some(transcript.path())), 10);
  }

  #[test]
  fn should_read_as_zero_when_the_transcript_does_not_exist_yet() {
    let transcript = Transcript::missing();

    assert_eq!(transcript_size(Some(transcript.path())), 0);
    assert_eq!(transcript_size(None), 0);
  }
}

mod poll {
  use super::*;

  #[test]
  fn should_work() {
    let a = Transcript::containing("0123456789");
    let b = Transcript::containing("01234567890123456789");
    let mut monitor = TranscriptMonitor::new(&[a.named("a"), b.named("b")]);
    a.append("01234");

    let line = monitor.poll(&[a.named("a"), b.named("b")]);

    assert_eq!(line.as_deref(), Some("transcripts grew: a +5"));
  }

  #[test]
  fn should_count_a_transcript_not_seen_before_as_growth_from_zero() {
    let a = Transcript::containing("0123456789");
    let mut monitor = TranscriptMonitor::new(&[a.named("a")]);
    let b = Transcript::containing("0123456");

    let line = monitor.poll(&[a.named("a"), b.named("b")]);

    assert_eq!(line.as_deref(), Some("transcripts grew: b +7"));
  }

  #[test]
  fn should_count_a_transcript_that_appears_after_the_first_look() {
    let a = Transcript::missing();
    let mut monitor = TranscriptMonitor::new(&[a.named("a")]);
    a.append("012");

    let line = monitor.poll(&[a.named("a")]);

    assert_eq!(line.as_deref(), Some("transcripts grew: a +3"));
  }

  #[test]
  fn should_report_each_growth_once() {
    let a = Transcript::containing("0123456789");
    let mut monitor = TranscriptMonitor::new(&[a.named("a")]);
    a.append("01234");
    monitor.poll(&[a.named("a")]);

    let line = monitor.poll(&[a.named("a")]);

    assert_eq!(line, None);
  }

  #[test]
  fn should_ignore_a_transcript_that_shrank_or_vanished() {
    let a = Transcript::containing("0123456789");
    let b = Transcript::containing("01234567890123456789");
    let mut monitor = TranscriptMonitor::new(&[a.named("a"), b.named("b")]);
    fs::write(a.path(), "0123").unwrap();
    b.remove();

    let line = monitor.poll(&[a.named("a")]);

    assert_eq!(line, None);
  }

  #[test]
  fn should_be_silent_when_nothing_grew() {
    let a = Transcript::containing("0123456789");
    let mut monitor = TranscriptMonitor::new(&[a.named("a")]);

    let line = monitor.poll(&[a.named("a")]);

    assert_eq!(line, None);
  }
}
