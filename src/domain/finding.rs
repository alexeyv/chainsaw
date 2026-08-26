use std::fmt;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use super::{require_nonblank, require_positive};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindingVerdict {
  Task,
  Dropped,
}

impl FindingVerdict {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Task => "task",
      Self::Dropped => "dropped",
    }
  }
}

impl fmt::Display for FindingVerdict {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(self.as_str())
  }
}

impl TryFrom<&str> for FindingVerdict {
  type Error = anyhow::Error;

  fn try_from(value: &str) -> Result<Self> {
    match value {
      "task" => Ok(Self::Task),
      "dropped" => Ok(Self::Dropped),
      value => bail!("unknown finding verdict {value:?}"),
    }
  }
}

/// A tracked concern requiring a verdict and reason before it is resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
  id: i64,
  task_id: i64,
  description: String,
  verdict: Option<FindingVerdict>,
  verdict_reason: Option<String>,
  fix_task_id: Option<i64>,
  created_at: DateTime<Utc>,
  resolved_at: Option<DateTime<Utc>>,
}

impl Finding {
  pub fn registered(
    id: i64,
    task_id: i64,
    description: String,
    created_at: DateTime<Utc>,
  ) -> Result<Self> {
    Self::from_record(id, task_id, description, None, None, None, created_at, None)
  }

  #[allow(clippy::too_many_arguments)]
  pub(crate) fn from_record(
    id: i64,
    task_id: i64,
    description: String,
    verdict: Option<FindingVerdict>,
    verdict_reason: Option<String>,
    fix_task_id: Option<i64>,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
  ) -> Result<Self> {
    require_positive("id", id)?;
    require_positive("task_id", task_id)?;
    require_nonblank("description", &description)?;
    if let Some(verdict_reason) = &verdict_reason {
      require_nonblank("verdict_reason", verdict_reason)?;
    }
    if let Some(fix_task_id) = fix_task_id {
      require_positive("fix_task_id", fix_task_id)?;
    }
    match (verdict, verdict_reason.as_ref(), fix_task_id, resolved_at) {
      (None, None, None, None) => {}
      (Some(FindingVerdict::Task), Some(_), Some(_), Some(_)) => {}
      (Some(FindingVerdict::Dropped), Some(_), None, Some(_)) => {}
      (Some(FindingVerdict::Task), _, None, _) => {
        bail!("task verdict requires a fix_task_id")
      }
      (Some(FindingVerdict::Dropped), _, Some(_), _) => {
        bail!("dropped verdict cannot have a fix_task_id")
      }
      (Some(_), None, _, _) => bail!("resolved finding requires a verdict_reason"),
      (Some(_), Some(_), _, None) => bail!("resolved finding requires a resolved_at"),
      (None, _, _, _) => bail!("unresolved finding cannot have resolution fields"),
    }

    Ok(Self {
      id,
      task_id,
      description,
      verdict,
      verdict_reason,
      fix_task_id,
      created_at,
      resolved_at,
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn task_id(&self) -> i64 {
    self.task_id
  }

  pub fn description(&self) -> &str {
    &self.description
  }

  pub fn verdict(&self) -> Option<FindingVerdict> {
    self.verdict
  }

  pub fn verdict_reason(&self) -> Option<&str> {
    self.verdict_reason.as_deref()
  }

  pub fn fix_task_id(&self) -> Option<i64> {
    self.fix_task_id
  }

  pub fn created_at(&self) -> DateTime<Utc> {
    self.created_at
  }

  pub fn resolved_at(&self) -> Option<DateTime<Utc>> {
    self.resolved_at
  }

  pub fn is_resolved(&self) -> bool {
    self.verdict.is_some()
  }
}

#[cfg(test)]
mod tests {
  use chrono::{DateTime, Utc};

  use super::{Finding, FindingVerdict};

  fn timestamp(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).unwrap()
  }

  fn finding_with(
    id: i64,
    task_id: i64,
    description: &str,
    verdict: FindingVerdict,
    verdict_reason: &str,
    fix_task_id: Option<i64>,
  ) -> anyhow::Result<Finding> {
    Finding::from_record(
      id,
      task_id,
      description.to_owned(),
      Some(verdict),
      Some(verdict_reason.to_owned()),
      fix_task_id,
      timestamp(1_700_000_000),
      Some(timestamp(1_700_000_001)),
    )
  }

  #[test]
  fn resolved_record_exposes_every_field_without_mutators() {
    let created_at = timestamp(1_700_000_000);
    let resolved_at = timestamp(1_700_000_001);
    let finding = Finding::from_record(
      3,
      5,
      "verification can accept the wrong commit".to_owned(),
      Some(FindingVerdict::Task),
      Some("the check trusts an ambiguous log entry".to_owned()),
      Some(7),
      created_at,
      Some(resolved_at),
    )
    .unwrap();

    assert_eq!(finding.id(), 3);
    assert_eq!(finding.task_id(), 5);
    assert_eq!(
      finding.description(),
      "verification can accept the wrong commit"
    );
    assert_eq!(finding.verdict(), Some(FindingVerdict::Task));
    assert_eq!(
      finding.verdict_reason(),
      Some("the check trusts an ambiguous log entry")
    );
    assert_eq!(finding.fix_task_id(), Some(7));
    assert_eq!(finding.created_at(), created_at);
    assert_eq!(finding.resolved_at(), Some(resolved_at));
    assert!(finding.is_resolved());
  }

  #[test]
  fn registered_finding_is_unresolved() {
    let created_at = timestamp(1_700_000_000);
    let finding = Finding::registered(3, 5, "a defect".to_owned(), created_at).unwrap();

    assert_eq!(finding.verdict(), None);
    assert_eq!(finding.verdict_reason(), None);
    assert_eq!(finding.fix_task_id(), None);
    assert_eq!(finding.resolved_at(), None);
    assert!(!finding.is_resolved());
  }

  #[test]
  fn verdicts_have_stable_storage_names_and_parse_from_them() {
    for (verdict, name) in [
      (FindingVerdict::Task, "task"),
      (FindingVerdict::Dropped, "dropped"),
    ] {
      assert_eq!(verdict.as_str(), name);
      assert_eq!(verdict.to_string(), verdict.as_str());
      assert_eq!(verdict.to_string(), name);
      assert_eq!(FindingVerdict::try_from(name).unwrap(), verdict);
    }

    let error = FindingVerdict::try_from("open").unwrap_err();
    assert_eq!(error.to_string(), "unknown finding verdict \"open\"");
  }

  #[test]
  fn constructor_requires_a_positive_id() {
    for id in [i64::MIN, -1, 0] {
      let error = finding_with(
        id,
        2,
        "a defect",
        FindingVerdict::Dropped,
        "not actionable",
        None,
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn constructor_requires_a_positive_task_id() {
    for task_id in [i64::MIN, -1, 0] {
      let error = finding_with(
        1,
        task_id,
        "a defect",
        FindingVerdict::Dropped,
        "not actionable",
        None,
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "task_id must be positive");
    }
  }

  #[test]
  fn constructor_requires_nonblank_text() {
    for description in ["", " ", "\n\t"] {
      let error = finding_with(
        1,
        2,
        description,
        FindingVerdict::Dropped,
        "not actionable",
        None,
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "description cannot be blank");
    }
    for verdict_reason in ["", " ", "\n\t"] {
      let error = finding_with(
        1,
        2,
        "a defect",
        FindingVerdict::Dropped,
        verdict_reason,
        None,
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "verdict_reason cannot be blank");
    }
  }

  #[test]
  fn constructor_requires_a_positive_fix_task_id() {
    for fix_task_id in [i64::MIN, -1, 0] {
      let error = finding_with(
        1,
        2,
        "a defect",
        FindingVerdict::Task,
        "worth fixing",
        Some(fix_task_id),
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "fix_task_id must be positive");
    }
  }

  #[test]
  fn constructor_matches_fix_tasks_to_verdicts() {
    let error =
      finding_with(1, 2, "a defect", FindingVerdict::Task, "worth fixing", None).unwrap_err();
    assert_eq!(error.to_string(), "task verdict requires a fix_task_id");

    let error = finding_with(
      1,
      2,
      "a defect",
      FindingVerdict::Dropped,
      "not actionable",
      Some(7),
    )
    .unwrap_err();
    assert_eq!(
      error.to_string(),
      "dropped verdict cannot have a fix_task_id"
    );
  }
}
