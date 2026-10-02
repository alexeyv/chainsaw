use super::{Finding, FindingVerdict};
use crate::domain::test_helpers::{created_at, finding_from_record, format_finding, resolved_at};

mod try_from {
  use super::*;

  #[test]
  fn should_work() {
    for (verdict, name) in [
      (FindingVerdict::Task, "task"),
      (FindingVerdict::Dropped, "dropped"),
    ] {
      assert_eq!(verdict.as_str(), name);
      assert_eq!(FindingVerdict::try_from(name).unwrap(), verdict);
    }
  }

  #[test]
  fn should_fail_when_the_name_is_unknown() {
    for name in ["open", "", "Task", "dropped "] {
      let error = FindingVerdict::try_from(name).unwrap_err();
      assert_eq!(
        error.to_string(),
        format!("unknown finding verdict {name:?}")
      );
    }
  }
}

mod from_record {
  use super::*;

  #[test]
  fn should_work() {
    let finding = Finding::from_record(
      3,
      5,
      "verification can accept the wrong commit".to_owned(),
      Some(FindingVerdict::Task),
      Some("the check trusts an ambiguous log entry".to_owned()),
      Some(7),
      created_at(),
      Some(resolved_at()),
    )
    .unwrap();

    assert_eq!(
      format_finding(&finding),
      r#"id: 3
task_id: 5
description: "verification can accept the wrong commit"
verdict: task
verdict_reason: "the check trusts an ambiguous log entry"
fix_task_id: 7
created_at: 2023-11-14T22:13:20Z
resolved_at: 2023-11-14T22:13:21Z
is_resolved: true"#
    );
  }

  #[test]
  fn should_accept_an_unresolved_record() {
    let finding = Finding::from_record(
      3,
      5,
      "a defect".to_owned(),
      None,
      None,
      None,
      created_at(),
      None,
    )
    .unwrap();

    assert_eq!(
      format_finding(&finding),
      r#"id: 3
task_id: 5
description: "a defect"
verdict: none
verdict_reason: none
fix_task_id: none
created_at: 2023-11-14T22:13:20Z
resolved_at: none
is_resolved: false"#
    );
  }

  #[test]
  fn should_accept_a_dropped_verdict_without_a_fix_task() {
    let finding = finding_from_record(
      3,
      5,
      "a defect",
      Some(FindingVerdict::Dropped),
      Some("not actionable"),
      None,
      Some(resolved_at()),
    )
    .unwrap();

    assert_eq!(
      format_finding(&finding),
      r#"id: 3
task_id: 5
description: "a defect"
verdict: dropped
verdict_reason: "not actionable"
fix_task_id: none
created_at: 2023-11-14T22:13:20Z
resolved_at: 2023-11-14T22:13:21Z
is_resolved: true"#
    );
  }

  #[test]
  fn should_fail_when_the_id_is_not_positive() {
    for id in [i64::MIN, -1, 0] {
      let error = finding_from_record(
        id,
        5,
        "a defect",
        Some(FindingVerdict::Dropped),
        Some("not actionable"),
        None,
        Some(resolved_at()),
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_task_id_is_not_positive() {
    for task_id in [i64::MIN, -1, 0] {
      let error = finding_from_record(
        3,
        task_id,
        "a defect",
        Some(FindingVerdict::Dropped),
        Some("not actionable"),
        None,
        Some(resolved_at()),
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "task_id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_description_is_blank() {
    for description in ["", " ", "\n\t"] {
      let error = finding_from_record(
        3,
        5,
        description,
        Some(FindingVerdict::Dropped),
        Some("not actionable"),
        None,
        Some(resolved_at()),
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "description cannot be blank");
    }
  }

  #[test]
  fn should_fail_when_the_verdict_reason_is_blank() {
    for verdict_reason in ["", " ", "\n\t"] {
      let error = finding_from_record(
        3,
        5,
        "a defect",
        Some(FindingVerdict::Dropped),
        Some(verdict_reason),
        None,
        Some(resolved_at()),
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "verdict_reason cannot be blank");
    }
  }

  #[test]
  fn should_fail_when_the_fix_task_id_is_not_positive() {
    for fix_task_id in [i64::MIN, -1, 0] {
      let error = finding_from_record(
        3,
        5,
        "a defect",
        Some(FindingVerdict::Task),
        Some("worth fixing"),
        Some(fix_task_id),
        Some(resolved_at()),
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "fix_task_id must be positive");
    }
  }

  #[test]
  fn should_fail_when_a_task_verdict_has_no_fix_task() {
    let error = finding_from_record(
      3,
      5,
      "a defect",
      Some(FindingVerdict::Task),
      Some("worth fixing"),
      None,
      Some(resolved_at()),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "task verdict requires a fix_task_id");
  }

  #[test]
  fn should_fail_when_a_dropped_verdict_has_a_fix_task() {
    let error = finding_from_record(
      3,
      5,
      "a defect",
      Some(FindingVerdict::Dropped),
      Some("not actionable"),
      Some(7),
      Some(resolved_at()),
    )
    .unwrap_err();
    assert_eq!(
      error.to_string(),
      "dropped verdict cannot have a fix_task_id"
    );
  }

  #[test]
  fn should_fail_when_a_resolved_finding_has_no_verdict_reason() {
    let task = finding_from_record(
      3,
      5,
      "a defect",
      Some(FindingVerdict::Task),
      None,
      Some(7),
      Some(resolved_at()),
    )
    .unwrap_err();
    let dropped = finding_from_record(
      3,
      5,
      "a defect",
      Some(FindingVerdict::Dropped),
      None,
      None,
      Some(resolved_at()),
    )
    .unwrap_err();

    assert_eq!(
      task.to_string(),
      "resolved finding requires a verdict_reason"
    );
    assert_eq!(
      dropped.to_string(),
      "resolved finding requires a verdict_reason"
    );
  }

  #[test]
  fn should_fail_when_a_resolved_finding_has_no_resolved_at() {
    let task = finding_from_record(
      3,
      5,
      "a defect",
      Some(FindingVerdict::Task),
      Some("worth fixing"),
      Some(7),
      None,
    )
    .unwrap_err();
    let dropped = finding_from_record(
      3,
      5,
      "a defect",
      Some(FindingVerdict::Dropped),
      Some("not actionable"),
      None,
      None,
    )
    .unwrap_err();

    assert_eq!(task.to_string(), "resolved finding requires a resolved_at");
    assert_eq!(
      dropped.to_string(),
      "resolved finding requires a resolved_at"
    );
  }

  #[test]
  fn should_fail_when_an_unresolved_finding_has_a_verdict_reason() {
    let error =
      finding_from_record(3, 5, "a defect", None, Some("premature"), None, None).unwrap_err();
    assert_eq!(
      error.to_string(),
      "unresolved finding cannot have resolution fields"
    );
  }

  #[test]
  fn should_fail_when_an_unresolved_finding_has_a_fix_task() {
    let error = finding_from_record(3, 5, "a defect", None, None, Some(7), None).unwrap_err();
    assert_eq!(
      error.to_string(),
      "unresolved finding cannot have resolution fields"
    );
  }

  #[test]
  fn should_fail_when_an_unresolved_finding_has_a_resolved_at() {
    let error =
      finding_from_record(3, 5, "a defect", None, None, None, Some(resolved_at())).unwrap_err();
    assert_eq!(
      error.to_string(),
      "unresolved finding cannot have resolution fields"
    );
  }
}
