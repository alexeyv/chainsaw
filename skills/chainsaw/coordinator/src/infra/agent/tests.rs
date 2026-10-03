use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use super::*;
use crate::domain::test_helpers::{
  FakeAgent, FakeSessionRuntime, RecordingSessionRuntime, start_request,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A transcript path nothing has written yet, removed when dropped.
struct Transcript(PathBuf);

impl Transcript {
  fn unwritten() -> Self {
    Self(std::env::temp_dir().join(format!(
      "chainsaw-agent-transcript-{}-{}.jsonl",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    )))
  }

  fn written() -> Self {
    let transcript = Self::unwritten();
    transcript.write();
    transcript
  }

  fn write(&self) {
    fs::write(&self.0, "").unwrap();
  }

  fn path(&self) -> PathBuf {
    self.0.clone()
  }
}

impl Drop for Transcript {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
  }
}

fn writing(transcript: &Transcript) -> FakeAgent {
  FakeAgent {
    transcript: Some(transcript.path()),
  }
}

mod open {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::written();

    let opened = open(&Claude, &transcript.path()).unwrap();

    assert_eq!(opened.path(), transcript.path());
  }

  #[test]
  fn should_be_none_when_there_is_no_file() {
    let transcript = Transcript::unwritten();

    assert!(open(&Claude, &transcript.path()).is_none());
  }
}

mod start_with_prompt {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::written();
    let runtime = RecordingSessionRuntime::default();
    let args = ["--model".to_owned(), "opus".to_owned()];

    let launched = start_with_prompt(
      &writing(&transcript),
      &runtime,
      start_request("worker", Path::new("/run"), &args),
      "Read the brief.",
    )
    .unwrap();

    assert_eq!(
      format!("{launched:?}"),
      format!(
        "Launched {{ started: StartedSession {{ external_id: \"external-worker\", pane_id: \"pane-worker\", tab_id: \"tab-worker\" }}, transcript: {:?} }}",
        transcript.path()
      )
    );
    assert_eq!(
      *runtime.started_args.borrow(),
      [["--model", "opus", "--", "Read the brief."]]
    );
  }

  #[test]
  fn should_fail_when_the_runtime_cannot_start_the_session() {
    let transcript = Transcript::written();
    let runtime = FakeSessionRuntime {
      status: None,
      reachable: true,
    };

    let error = start_with_prompt(
      &writing(&transcript),
      &runtime,
      start_request("worker", Path::new("/run"), &[]),
      "Read the brief.",
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "the fake runtime starts nothing");
  }
}

mod transcript_within {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::written();

    let found = transcript_within(
      &writing(&transcript),
      Path::new("/run"),
      "external-worker",
      Duration::ZERO,
    )
    .unwrap();

    assert_eq!(found, transcript.path());
  }

  #[test]
  fn should_wait_for_a_transcript_the_agent_begins_late() {
    let transcript = Transcript::unwritten();
    let agent = writing(&transcript);

    let found = thread::scope(|scope| {
      scope.spawn(|| {
        thread::sleep(Duration::from_millis(100));
        transcript.write();
      });
      transcript_within(
        &agent,
        Path::new("/run"),
        "external-worker",
        Duration::from_secs(10),
      )
    })
    .unwrap();

    assert_eq!(found, transcript.path());
  }

  #[test]
  fn should_fail_when_the_agent_writes_none_in_time() {
    let transcript = Transcript::unwritten();

    let error = transcript_within(
      &writing(&transcript),
      Path::new("/run"),
      "external-worker",
      Duration::ZERO,
    )
    .unwrap_err();

    assert_eq!(
      error.to_string(),
      "fake-agent session external-worker wrote no transcript within 0 seconds"
    );
  }
}
