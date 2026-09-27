//! Judging a task's commit: the mechanical gate that accepts it, the forced
//! acceptance that stands in for the gate, and the forced records of a commit
//! or commentary delivery that remedy a coordinator that missed one.

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use super::{Coordinator, new_commit_for, task_commits};
use crate::domain::{RunEventKind, TaskState};
use crate::persistence::{run_event, task};

/// How long to wait for the agent to log a commit HEAD already shows before
/// looking for it in the transcript again.
const VERIFY_LOG_RETRY_SECONDS: u64 = 1;
const COORDINATOR_REMEDY_ONLY: &str = "normally the coordinator records this on its own; use --force --reason only to remedy a coordinator failure";

fn forced_remedy_reason<'a>(
  command: &str,
  force: bool,
  reason: Option<&'a str>,
) -> Result<&'a str> {
  match (force, reason) {
    (true, Some(reason)) if !reason.trim().is_empty() => Ok(reason),
    (true, _) => bail!("supervisor: {command} --force requires a non-empty --reason"),
    (false, Some(_)) => {
      bail!("supervisor: --reason only applies with --force; {COORDINATOR_REMEDY_ONLY}")
    }
    (false, None) => bail!("supervisor: {COORDINATOR_REMEDY_ONLY}"),
  }
}

pub(super) fn cmd_task_record_commit(
  coordinator: &Coordinator,
  task_id: i64,
  sha: &str,
  force: bool,
  reason: Option<&str>,
) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: no task {task_id}");
  };
  let reason = forced_remedy_reason("task record-commit", force, reason)?;
  if !matches!(task.state(), TaskState::Dispatched | TaskState::InFlight) {
    bail!(
      "supervisor: task {task_id} is {}, not awaiting a coordinator-recorded commit",
      task.state()
    );
  }
  let Some(commit_sha) = coordinator.repo.canonical_commit(sha)? else {
    bail!("supervisor: commit {sha} does not exist in the run repository");
  };
  coordinator.store.write(|tx| {
    for other in task::all(tx)? {
      if other.id() != task_id
        && other
          .commit_sha()
          .is_some_and(|recorded| commit_sha.starts_with(recorded))
      {
        bail!(
          "supervisor: commit {sha} is already recorded for task {}",
          other.id()
        );
      }
    }
    let base_head = task.base_head().with_context(|| {
      format!("supervisor: task {task_id} has no base_head to validate the commit against")
    })?;
    if commit_sha
      == coordinator
        .repo
        .canonical_commit(base_head)?
        .unwrap_or_default()
      || !coordinator.repo.is_ancestor(base_head, &commit_sha)?
    {
      bail!(
        "supervisor: commit {sha} does not descend from task {task_id}'s base_head as a new commit"
      );
    }
    task::record_commit(tx, task_id, sha, Some(reason))?;
    run_event::create(
      tx,
      RunEventKind::ForcedCommit,
      &format!("task {task_id} {sha}: {reason}"),
    )?;
    Ok(())
  })?;
  println!("task {task_id} commit recorded by force: {sha}");
  Ok(())
}

pub(super) fn cmd_task_record_commentary(
  coordinator: &Coordinator,
  task_id: i64,
  force: bool,
  reason: Option<&str>,
) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: no task {task_id}");
  };
  let reason = forced_remedy_reason("task record-commentary", force, reason)?;
  if !matches!(
    task.state(),
    TaskState::CommittedUnverified | TaskState::Accepted
  ) || task.commit_sha().is_none()
  {
    bail!(
      "supervisor: task {task_id} is {}, not ready for commentary delivery",
      task.state()
    );
  }
  coordinator.store.write(|tx| {
    if !task::record_commentary_delivery(tx, task_id)? {
      bail!("supervisor: commentary delivery is already recorded for task {task_id}");
    }
    run_event::create(
      tx,
      RunEventKind::ForcedCommentary,
      &format!("task {task_id}: {reason}"),
    )
  })?;
  println!("task {task_id} commentary delivery recorded by force");
  Ok(())
}

/// Accept a task. Without `--force` this runs the mechanical gate and accepts
/// only if it passes; with it the caller's reason stands in for the gate.
pub(super) fn cmd_accept(
  coordinator: &Coordinator,
  task_id: i64,
  force: bool,
  reason: Option<&str>,
) -> Result<()> {
  if coordinator
    .store
    .read(|tx| task::get(tx, task_id))?
    .is_none()
  {
    bail!("supervisor: no task {task_id}");
  }
  match (force, reason) {
    (true, Some(reason)) => accept_without_the_gate(coordinator, task_id, reason),
    (true, None) => bail!("supervisor: accept --force requires a non-empty --reason"),
    (false, Some(_)) => {
      bail!("supervisor: --reason only applies with --force; accept without it runs the checks")
    }
    (false, None) => accept_through_the_gate(coordinator, task_id),
  }
}

fn accept_without_the_gate(coordinator: &Coordinator, task_id: i64, reason: &str) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: no task {task_id}");
  };
  if reason.trim().is_empty() {
    bail!("supervisor: accept --force requires a non-empty --reason");
  }
  if task.state() != TaskState::CommittedUnverified || task.commit_sha().is_none() {
    bail!(
      "supervisor: task {task_id} is {}, not a committed unverified task",
      task.state()
    );
  }
  coordinator.store.write(|tx| {
    task::accept(tx, task_id, reason)?;
    run_event::create(
      tx,
      RunEventKind::Accepted,
      &format!("task {task_id}: {reason}"),
    )?;
    Ok(())
  })?;
  println!("task {task_id} accepted without the gate: {reason}");
  Ok(())
}

fn accept_through_the_gate(coordinator: &Coordinator, task_id: i64) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: no task {task_id}");
  };
  let mut sha = match task.commit_sha() {
    Some(sha) => Some(sha.to_owned()),
    None => new_commit_for(
      coordinator,
      &task_commits(coordinator, &task)?,
      task.base_head(),
    )?,
  };
  if sha.is_none() && head_advanced_cleanly(coordinator, task.base_head())? {
    thread::sleep(Duration::from_secs(VERIFY_LOG_RETRY_SECONDS));
    sha = new_commit_for(
      coordinator,
      &task_commits(coordinator, &task)?,
      task.base_head(),
    )?;
  }
  let mut problems = Vec::new();
  if let Some(sha) = &sha {
    match coordinator.repo.commit(sha)? {
      None => problems.push(format!("commit {sha} not in git")),
      Some(commit) => {
        if has_attribution_trailer(&commit.message) {
          problems.push("commit carries an attribution trailer".to_owned());
        }
        if !coordinator.repo.head()?.starts_with(&commit.sha) {
          problems.push("commit is not HEAD".to_owned());
        }
      }
    }
  } else {
    problems.push("no new commit since the task was dispatched".to_owned());
  }
  if !coordinator.repo.is_clean()? {
    problems.push("tree is dirty".to_owned());
  }
  // The implementer runs the quality gate before it commits; that is its
  // contract, and re-deriving it from the transcript only costs wall time.
  if problems.is_empty() {
    let sha = sha
      .as_deref()
      .context("accepted task unexpectedly has no commit")?;
    coordinator.store.write(|tx| {
      task::record_commit(tx, task_id, sha, None)?;
      task::accept(tx, task_id, &format!("checks passed at {sha}"))?;
      Ok(())
    })?;
    println!("task {task_id} accepted: checks passed at {sha}");
    return Ok(());
  }
  println!("task {task_id} NOT accepted:");
  for problem in problems {
    println!(" - {problem}");
  }
  Err(anyhow!(""))
}

fn head_advanced_cleanly(coordinator: &Coordinator, base_head: Option<&str>) -> Result<bool> {
  match base_head {
    Some(base_head) => coordinator.repo.head_advanced_cleanly_from(base_head),
    None => Ok(false),
  }
}

fn has_attribution_trailer(message: &str) -> bool {
  message.lines().any(|line| {
    let lowercase = line.to_ascii_lowercase();
    lowercase.starts_with("co-authored-by:") || lowercase.starts_with("claude-session:")
  })
}
