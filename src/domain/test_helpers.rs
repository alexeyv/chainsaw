use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};

use super::{Finding, FindingVerdict};

pub fn created_at() -> DateTime<Utc> {
  DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

pub fn resolved_at() -> DateTime<Utc> {
  DateTime::from_timestamp(1_700_000_001, 0).unwrap()
}

pub fn finding_from_record(
  id: i64,
  task_id: i64,
  description: &str,
  verdict: Option<FindingVerdict>,
  verdict_reason: Option<&str>,
  fix_task_id: Option<i64>,
  resolved_at: Option<DateTime<Utc>>,
) -> Result<Finding> {
  Finding::from_record(
    id,
    task_id,
    description.to_owned(),
    verdict,
    verdict_reason.map(str::to_owned),
    fix_task_id,
    created_at(),
    resolved_at,
  )
}

pub fn format_finding(finding: &Finding) -> String {
  let verdict = match finding.verdict() {
    Some(verdict) => verdict.to_string(),
    None => "none".to_owned(),
  };
  let verdict_reason = match finding.verdict_reason() {
    Some(reason) => format!("{reason:?}"),
    None => "none".to_owned(),
  };
  let fix_task_id = match finding.fix_task_id() {
    Some(id) => id.to_string(),
    None => "none".to_owned(),
  };
  let created_at = finding
    .created_at()
    .to_rfc3339_opts(SecondsFormat::Secs, true);
  let resolved_at = match finding.resolved_at() {
    Some(time) => time.to_rfc3339_opts(SecondsFormat::Secs, true),
    None => "none".to_owned(),
  };
  format!(
    "id: {}\ntask_id: {}\ndescription: {:?}\nverdict: {}\nverdict_reason: {}\nfix_task_id: {}\ncreated_at: {}\nresolved_at: {}\nis_resolved: {}",
    finding.id(),
    finding.task_id(),
    finding.description(),
    verdict,
    verdict_reason,
    fix_task_id,
    created_at,
    resolved_at,
    finding.is_resolved(),
  )
}
