use crate::domain::Role;
use crate::domain::test_helpers::{
  SessionSpec, build_session, format_session, launched_implementer, timestamp, working_implementer,
};

mod role_try_from {
  use super::*;

  #[test]
  fn should_work() {
    for (text, role) in [
      ("lead", Role::Lead),
      ("implementer", Role::Implementer),
      ("commentator", Role::Commentator),
    ] {
      assert_eq!(Role::try_from(text).unwrap(), role);
      assert_eq!(role.to_string(), text);
    }
  }

  #[test]
  fn should_fail_when_the_role_is_unknown() {
    let error = Role::try_from("reviewer").unwrap_err();
    assert_eq!(error.to_string(), "unknown session role \"reviewer\"");
  }
}

mod new {
  use super::*;

  #[test]
  fn should_work() {
    let session = build_session(working_implementer()).unwrap();

    assert_eq!(
      format_session(&session),
      r#"id: 7
name: "implementer-1"
role: implementer
external_session_id: "0b5c2e6a-1d3f-4a8b-9c7e-2f1a3b4c5d6e"
launched_head: "base123"
started_at: 2023-11-14T22:13:20Z
stopped_at: none
context: 4000
context_max: 5000
last_growth: 2023-11-14T22:23:20Z
kicked_at: none
is_live: true
can_take_task: true
can_be_kicked: true"#
    );
  }

  #[test]
  fn should_accept_a_session_that_has_just_launched() {
    let session = build_session(launched_implementer()).unwrap();

    assert_eq!(
      format_session(&session),
      r#"id: 7
name: "implementer-1"
role: implementer
external_session_id: "0b5c2e6a-1d3f-4a8b-9c7e-2f1a3b4c5d6e"
launched_head: "base123"
started_at: 2023-11-14T22:13:20Z
stopped_at: none
context: 0
context_max: 0
last_growth: 2023-11-14T22:13:20Z
kicked_at: none
is_live: true
can_take_task: true
can_be_kicked: true"#
    );
  }

  #[test]
  fn should_accept_a_lead_without_a_launch_head() {
    let session = build_session(SessionSpec {
      name: "lead",
      role: Role::Lead,
      launched_head: None,
      ..working_implementer()
    })
    .unwrap();

    assert_eq!(session.role(), Role::Lead);
    assert_eq!(session.launched_head(), None);
    assert!(!session.can_take_task());
  }

  #[test]
  fn should_accept_a_stopped_and_kicked_session() {
    let session = build_session(SessionSpec {
      stopped_at: Some(timestamp(1_700_001_000)),
      kicked_at: Some(timestamp(1_700_000_900)),
      ..working_implementer()
    })
    .unwrap();

    assert_eq!(
      format_session(&session),
      r#"id: 7
name: "implementer-1"
role: implementer
external_session_id: "0b5c2e6a-1d3f-4a8b-9c7e-2f1a3b4c5d6e"
launched_head: "base123"
started_at: 2023-11-14T22:13:20Z
stopped_at: 2023-11-14T22:30:00Z
context: 4000
context_max: 5000
last_growth: 2023-11-14T22:23:20Z
kicked_at: 2023-11-14T22:28:20Z
is_live: false
can_take_task: false
can_be_kicked: false"#
    );
  }

  #[test]
  fn should_accept_stopping_and_kicking_at_the_start_instant() {
    let session = build_session(SessionSpec {
      stopped_at: Some(timestamp(1_700_000_000)),
      kicked_at: Some(timestamp(1_700_000_000)),
      ..launched_implementer()
    })
    .unwrap();

    assert_eq!(session.stopped_at(), Some(timestamp(1_700_000_000)));
    assert_eq!(session.kicked_at(), Some(timestamp(1_700_000_000)));
  }

  #[test]
  fn should_fail_when_the_id_is_not_positive() {
    for id in [i64::MIN, -1, 0] {
      let error = build_session(SessionSpec {
        id,
        ..working_implementer()
      })
      .unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_name_is_blank() {
    for name in ["", "  \t"] {
      let error = build_session(SessionSpec {
        name,
        ..working_implementer()
      })
      .unwrap_err();
      assert_eq!(error.to_string(), "name cannot be blank");
    }
  }

  #[test]
  fn should_fail_when_the_external_session_id_is_blank() {
    let error = build_session(SessionSpec {
      external_session_id: " ",
      ..working_implementer()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "external_session_id cannot be blank");
  }

  #[test]
  fn should_fail_when_the_launch_head_is_blank() {
    let error = build_session(SessionSpec {
      launched_head: Some(""),
      ..working_implementer()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "launched_head cannot be blank");
  }

  #[test]
  fn should_fail_when_a_context_reading_is_negative() {
    let context = build_session(SessionSpec {
      context: -1,
      ..working_implementer()
    })
    .unwrap_err();
    let context_max = build_session(SessionSpec {
      context_max: -1,
      ..launched_implementer()
    })
    .unwrap_err();

    assert_eq!(context.to_string(), "context cannot be negative");
    assert_eq!(context_max.to_string(), "context_max cannot be negative");
  }

  #[test]
  fn should_fail_when_the_maximum_is_below_the_current_context() {
    let error = build_session(SessionSpec {
      context: 5_001,
      ..working_implementer()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "context_max cannot be below context");
  }

  #[test]
  fn should_fail_when_the_session_stopped_before_it_started() {
    let error = build_session(SessionSpec {
      stopped_at: Some(timestamp(1_699_999_999)),
      ..working_implementer()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "stopped_at cannot precede started_at");
  }

  #[test]
  fn should_fail_when_the_transcript_grew_before_the_session_started() {
    let error = build_session(SessionSpec {
      last_growth: timestamp(1_699_999_999),
      ..working_implementer()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "last_growth cannot precede started_at");
  }

  #[test]
  fn should_fail_when_the_session_was_kicked_before_it_started() {
    let error = build_session(SessionSpec {
      kicked_at: Some(timestamp(1_699_999_999)),
      ..working_implementer()
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "kicked_at cannot precede started_at");
  }
}

mod can_take_task {
  use super::*;

  #[test]
  fn should_work() {
    let session = build_session(working_implementer()).unwrap();
    assert!(session.can_take_task());
  }

  #[test]
  fn should_refuse_when_the_session_is_not_an_implementer() {
    for role in [Role::Lead, Role::Commentator] {
      let session = build_session(SessionSpec {
        role,
        ..working_implementer()
      })
      .unwrap();
      assert!(!session.can_take_task(), "{role} took a task");
    }
  }

  #[test]
  fn should_refuse_when_the_implementer_is_stopped() {
    let session = build_session(SessionSpec {
      stopped_at: Some(timestamp(1_700_001_000)),
      ..working_implementer()
    })
    .unwrap();
    assert!(!session.can_take_task());
  }
}

mod quiet_seconds {
  use super::*;

  #[test]
  fn should_work() {
    let session = build_session(working_implementer()).unwrap();
    assert_eq!(session.quiet_seconds(timestamp(1_700_000_725)), 125);
  }

  #[test]
  fn should_read_as_just_now_when_the_clock_is_behind_the_last_growth() {
    let session = build_session(working_implementer()).unwrap();
    assert_eq!(session.quiet_seconds(timestamp(1_700_000_599)), 0);
  }
}

mod can_be_kicked {
  use super::*;

  #[test]
  fn should_work() {
    let session = build_session(working_implementer()).unwrap();
    assert!(session.can_be_kicked());
  }

  #[test]
  fn should_refuse_when_the_session_was_already_kicked() {
    let session = build_session(SessionSpec {
      kicked_at: Some(timestamp(1_700_000_900)),
      ..working_implementer()
    })
    .unwrap();
    assert!(!session.can_be_kicked());
  }

  #[test]
  fn should_refuse_when_the_session_is_stopped() {
    let session = build_session(SessionSpec {
      stopped_at: Some(timestamp(1_700_001_000)),
      ..working_implementer()
    })
    .unwrap();
    assert!(!session.can_be_kicked());
  }
}
