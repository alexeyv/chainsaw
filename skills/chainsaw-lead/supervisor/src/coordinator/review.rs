//! The channel between the commentator and the lead: observations that
//! need no answer, findings that need a verdict, and the poll and resolution
//! commands each side reads and writes them through.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;

use super::Coordinator;
use crate::cli::Verdict;
use crate::domain::{FindingVerdict, Task};
use crate::persistence::{finding, observation, task};

pub(super) fn cmd_observe(
  coordinator: &Coordinator,
  task_id: Option<i64>,
  text: &str,
) -> Result<()> {
  let observation = coordinator.store.write(|tx| {
    if let Some(task_id) = task_id {
      require_task(tx, task_id)?;
    }
    observation::create(tx, task_id, text)
  })?;
  println!("{}", observation.id());
  Ok(())
}

pub(super) fn cmd_finding(
  coordinator: &Coordinator,
  task_id: i64,
  description: &str,
) -> Result<()> {
  let finding = coordinator.store.write(|tx| {
    require_task(tx, task_id)?;
    finding::register(tx, task_id, description)
  })?;
  println!("{}", finding.id());
  Ok(())
}

pub(super) fn cmd_poll(
  coordinator: &Coordinator,
  after_observation: i64,
  task_id: Option<i64>,
) -> Result<()> {
  if after_observation < 0 {
    bail!("supervisor: --after-observation must be nonnegative");
  }
  let (observations, findings) = coordinator.store.read(|tx| {
    if let Some(task_id) = task_id {
      require_task(tx, task_id)?;
    }
    Ok((
      observation::after(tx, after_observation, task_id)?,
      finding::unresolved(tx, task_id)?,
    ))
  })?;
  let observation_cursor = observations
    .last()
    .map_or(after_observation, |observation| observation.id());
  let observations = observations
    .into_iter()
    .map(|observation| {
      json!({
        "id": observation.id(),
        "task_id": observation.task_id(),
        "text": observation.text(),
        "created_at": observation.created_at().to_rfc3339(),
      })
    })
    .collect::<Vec<_>>();
  let findings = findings
    .into_iter()
    .map(|finding| {
      json!({
        "id": finding.id(),
        "task_id": finding.task_id(),
        "description": finding.description(),
        "created_at": finding.created_at().to_rfc3339(),
      })
    })
    .collect::<Vec<_>>();
  println!(
    "{}",
    json!({
      "observation_cursor": observation_cursor,
      "observations": observations,
      "findings": findings,
    })
  );
  Ok(())
}

pub(super) fn cmd_resolve(
  coordinator: &Coordinator,
  finding_id: i64,
  verdict: &Verdict,
  fix_task_id: Option<i64>,
  reason: &str,
) -> Result<()> {
  let verdict = match verdict {
    Verdict::Task => FindingVerdict::Task,
    Verdict::Dropped => FindingVerdict::Dropped,
  };
  coordinator.store.write(|tx| {
    let finding = finding::get(tx, finding_id)?
      .with_context(|| format!("supervisor: no finding {finding_id}"))?;
    if let Some(fix_task_id) = fix_task_id {
      require_task(tx, fix_task_id)?;
    }
    finding::resolve(tx, &finding, verdict, reason, fix_task_id)
      .map_err(|error| anyhow!("supervisor: {error}"))?;
    Ok(())
  })?;
  println!("finding {finding_id} resolved");
  Ok(())
}

pub(super) fn cmd_resolutions(coordinator: &Coordinator) -> Result<()> {
  let resolutions = coordinator
    .store
    .read(finding::resolved)?
    .into_iter()
    .map(|finding| {
      json!({
        "finding_id": finding.id(),
        "task_id": finding.task_id(),
        "description": finding.description(),
        "verdict": finding.verdict().map(FindingVerdict::as_str),
        "reason": finding.verdict_reason(),
        "fix_task_id": finding.fix_task_id(),
        "resolved_at": finding.resolved_at().map(|time| time.to_rfc3339()),
      })
    })
    .collect::<Vec<_>>();
  println!("{}", json!({"resolutions": resolutions}));
  Ok(())
}

fn require_task(transaction: &rusqlite::Transaction<'_>, task_id: i64) -> Result<Task> {
  task::get(transaction, task_id)?.with_context(|| format!("supervisor: no task {task_id}"))
}
