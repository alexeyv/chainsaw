use strum::IntoEnumIterator;

use crate::domain::RunEventKind;
use crate::domain::test_helpers::{format_run_event, run_event};

mod try_from {
  use super::*;

  #[test]
  fn should_work() {
    let names = [
      (RunEventKind::Launch, "launch"),
      (RunEventKind::PromptQueued, "prompt-queued"),
      (RunEventKind::PromptTaken, "prompt-taken"),
      (RunEventKind::PromptFailed, "prompt-failed"),
      (RunEventKind::PromptUnreachable, "prompt-unreachable"),
      (RunEventKind::Dispatch, "dispatch"),
      (RunEventKind::DispatchFailed, "dispatch-failed"),
      (RunEventKind::Committed, "committed"),
      (RunEventKind::ForcedCommit, "forced-commit"),
      (RunEventKind::ForcedCommentary, "forced-commentary"),
      (RunEventKind::CommentaryWake, "commentary-wake"),
      (RunEventKind::CommentaryDelivered, "commentary-delivered"),
      (RunEventKind::Accepted, "accepted"),
      (RunEventKind::Aborted, "aborted"),
      (RunEventKind::AbortInterrupt, "abort-interrupt"),
      (RunEventKind::AbortUnreachable, "abort-unreachable"),
      (RunEventKind::Kick, "kick"),
      (RunEventKind::Compact, "compact"),
      (RunEventKind::StopLead, "stop-lead"),
      (RunEventKind::Stop, "stop"),
      (RunEventKind::DaemonStart, "daemon-start"),
      (RunEventKind::DaemonExit, "daemon-exit"),
      (RunEventKind::TranscriptMissing, "transcript-missing"),
      (RunEventKind::TranscriptFound, "transcript-found"),
    ];

    assert_eq!(names.len(), RunEventKind::iter().count());
    for (kind, name) in names {
      assert_eq!(kind.as_str(), name);
      assert_eq!(kind.to_string(), name);
      assert_eq!(RunEventKind::try_from(name).unwrap(), kind);
    }
  }

  #[test]
  fn should_fail_when_the_name_is_unknown() {
    for name in ["", "Launch", "stop_lead", "kick "] {
      let error = RunEventKind::try_from(name).unwrap_err();
      assert_eq!(
        error.to_string(),
        format!("unknown run event kind {name:?}")
      );
    }
  }
}

mod new {
  use super::*;

  #[test]
  fn should_work() {
    let event = run_event(3, RunEventKind::Kick, "implementer-1").unwrap();

    assert_eq!(
      format_run_event(&event),
      r#"id: 3
kind: kick
detail: "implementer-1"
created_at: 2023-11-14T22:13:20Z"#
    );
  }

  #[test]
  fn should_fail_when_the_id_is_not_positive() {
    for id in [i64::MIN, -1, 0] {
      let error = run_event(id, RunEventKind::Kick, "implementer-1").unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_detail_is_blank() {
    for detail in ["", " ", "\n\t"] {
      let error = run_event(3, RunEventKind::Kick, detail).unwrap_err();
      assert_eq!(error.to_string(), "detail cannot be blank");
    }
  }
}
