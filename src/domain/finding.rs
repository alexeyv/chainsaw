use std::fmt;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

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

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
  id: i64,
  description: String,
  verdict: FindingVerdict,
  verdict_reason: String,
  fix_task_id: Option<i64>,
  created_at: DateTime<Utc>,
}

impl Finding {
  pub fn new(
    id: i64,
    description: String,
    verdict: FindingVerdict,
    verdict_reason: String,
    fix_task_id: Option<i64>,
    created_at: DateTime<Utc>,
  ) -> Result<Self> {
    require_positive("id", id)?;
    require_nonblank("description", &description)?;
    require_nonblank("verdict_reason", &verdict_reason)?;
    if let Some(fix_task_id) = fix_task_id {
      require_positive("fix_task_id", fix_task_id)?;
    }
    match (verdict, fix_task_id) {
      (FindingVerdict::Task, None) => bail!("task verdict requires a fix_task_id"),
      (FindingVerdict::Dropped, Some(_)) => bail!("dropped verdict cannot have a fix_task_id"),
      _ => {}
    }

    Ok(Self {
      id,
      description,
      verdict,
      verdict_reason,
      fix_task_id,
      created_at,
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn description(&self) -> &str {
    &self.description
  }

  pub fn verdict(&self) -> FindingVerdict {
    self.verdict
  }

  pub fn verdict_reason(&self) -> &str {
    &self.verdict_reason
  }

  pub fn fix_task_id(&self) -> Option<i64> {
    self.fix_task_id
  }

  pub fn created_at(&self) -> DateTime<Utc> {
    self.created_at
  }
}

fn require_positive(field: &'static str, value: i64) -> Result<()> {
  if value <= 0 {
    bail!("{field} must be positive");
  }
  Ok(())
}

fn require_nonblank(field: &'static str, value: &str) -> Result<()> {
  if value.trim().is_empty() {
    bail!("{field} cannot be blank");
  }
  Ok(())
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
    description: &str,
    verdict: FindingVerdict,
    verdict_reason: &str,
    fix_task_id: Option<i64>,
  ) -> anyhow::Result<Finding> {
    Finding::new(
      id,
      description.to_owned(),
      verdict,
      verdict_reason.to_owned(),
      fix_task_id,
      timestamp(1_700_000_000),
    )
  }

  #[test]
  fn constructor_exposes_every_field_without_mutators() {
    let created_at = timestamp(1_700_000_000);
    let finding = Finding::new(
      3,
      "verification can accept the wrong commit".to_owned(),
      FindingVerdict::Task,
      "the check trusts an ambiguous log entry".to_owned(),
      Some(7),
      created_at,
    )
    .unwrap();

    assert_eq!(finding.id(), 3);
    assert_eq!(
      finding.description(),
      "verification can accept the wrong commit"
    );
    assert_eq!(finding.verdict(), FindingVerdict::Task);
    assert_eq!(
      finding.verdict_reason(),
      "the check trusts an ambiguous log entry"
    );
    assert_eq!(finding.fix_task_id(), Some(7));
    assert_eq!(finding.created_at(), created_at);
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
  fn constructor_requires_nonblank_text() {
    for description in ["", " ", "\n\t"] {
      let error = finding_with(
        1,
        description,
        FindingVerdict::Dropped,
        "not actionable",
        None,
      )
      .unwrap_err();
      assert_eq!(error.to_string(), "description cannot be blank");
    }
    for verdict_reason in ["", " ", "\n\t"] {
      let error =
        finding_with(1, "a defect", FindingVerdict::Dropped, verdict_reason, None).unwrap_err();
      assert_eq!(error.to_string(), "verdict_reason cannot be blank");
    }
  }

  #[test]
  fn constructor_requires_a_positive_fix_task_id() {
    for fix_task_id in [i64::MIN, -1, 0] {
      let error = finding_with(
        1,
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
      finding_with(1, "a defect", FindingVerdict::Task, "worth fixing", None).unwrap_err();
    assert_eq!(error.to_string(), "task verdict requires a fix_task_id");

    let error = finding_with(
      1,
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
