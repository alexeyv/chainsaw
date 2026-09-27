//! A task's way through the run at the lead's hand: drafted from text on
//! stdin, dispatched to a fresh implementer with the contract appended, and
//! aborted when it will not produce an accepted commit. Also how the
//! supervisor tells which commit, if any, a task's implementer made.

use std::io::Read;

use anyhow::{Context, Result, bail};

use super::{
  Coordinator, cmd_prompt, daemon_prompt, record_run_event, session_name, session_transcript,
  task_session,
};
use crate::domain::{RunEventKind, Session, Task, TaskState};
use crate::infra::agent;
use crate::infra::transcript_monitor::transcript_size;
use crate::persistence::{run_event, session, task};

const CONTRACT: &str = "Verify the tree is clean; stop if dirty. Implement only this task. Run the task's checks as you work; run the project's quality gate once, immediately before committing. Commit without attribution trailers, leave the tree clean, then run exactly `git log -1 --format='[chainsaw %h]'` (the supervisor reads that record), and finish with the commit id, changed-file manifest, a one-paragraph semantic delta, and any gate failures you judged pre-existing (test name and one-line error).";

/// Commit ids the task's session may have made since the task was dispatched;
/// `new_commit_for` decides whether one is really new.
pub(super) fn task_commits(coordinator: &Coordinator, task: &Task) -> Result<Vec<String>> {
  let Some(session) = task_session(coordinator, task)? else {
    return Ok(Vec::new());
  };
  let Some(transcript) = session_transcript(coordinator, &session)? else {
    return Ok(Vec::new());
  };
  let head = coordinator.repo.head()?;
  Ok(agent::for_session(&session).commit_candidates(
    &transcript,
    task.transcript_offset() as u64,
    &head,
  ))
}

fn last_task_on(coordinator: &Coordinator, session_id: i64) -> Result<Option<Task>> {
  Ok(
    coordinator
      .store
      .read(|tx| task::tasks_for_session(tx, session_id))?
      .into_iter()
      .rev()
      .find(|task| task.state() != TaskState::Drafted),
  )
}

/// What `task new` was given on its command line; the task text comes on stdin.
pub(super) struct NewTaskOptions<'a> {
  pub(super) predicted_files: Option<i64>,
  pub(super) predicted_lines: i64,
  pub(super) retry_of_task_id: Option<i64>,
  pub(super) files: Option<&'a str>,
  pub(super) reason: Option<&'a str>,
}

pub(super) fn cmd_task_new(coordinator: &Coordinator, options: NewTaskOptions<'_>) -> Result<()> {
  let NewTaskOptions {
    mut predicted_files,
    predicted_lines,
    retry_of_task_id,
    files,
    reason,
  } = options;
  let mut text = String::new();
  std::io::stdin().read_to_string(&mut text)?;
  if text.trim().is_empty() {
    bail!("supervisor: task text on stdin is empty");
  }
  let active_retry = match retry_of_task_id {
    Some(retry_of_task_id) => {
      let predecessor = coordinator
        .store
        .read(|tx| task::get(tx, retry_of_task_id))?
        .with_context(|| {
          format!(
            "supervisor: --retry-of {retry_of_task_id} is not aborted, dispatched, or in flight"
          )
        })?;
      match predecessor.state() {
        TaskState::Aborted => None,
        TaskState::Dispatched | TaskState::InFlight => {
          let reason = reason.filter(|reason| !reason.trim().is_empty()).with_context(|| {
            format!(
              "supervisor: --retry-of {retry_of_task_id} requires a non-empty --reason while the predecessor is {}",
              predecessor.state()
            )
          })?;
          Some((retry_of_task_id, reason))
        }
        _ => bail!(
          "supervisor: --retry-of {retry_of_task_id} is not aborted, dispatched, or in flight"
        ),
      }
    }
    None => None,
  };
  let file_list: Vec<_> = files
    .unwrap_or_default()
    .split(',')
    .map(str::trim)
    .filter(|file| !file.is_empty())
    .collect();
  if !file_list.is_empty() {
    let count = i64::try_from(file_list.len()).unwrap_or(i64::MAX);
    if predicted_files.is_some_and(|predicted| predicted != count) {
      bail!(
        "supervisor: --predicted-files {} disagrees with --files ({count} names); give one or the other",
        predicted_files.unwrap_or_default()
      );
    }
    predicted_files = Some(count);
  } else if predicted_files.is_none() {
    bail!("supervisor: task new needs --files a,b,c or --predicted-files N");
  }
  let predicted_files = predicted_files.context("task file prediction was not validated")?;
  let predicted_file_list =
    (!file_list.is_empty()).then(|| file_list.into_iter().map(str::to_owned).collect::<Vec<_>>());
  if let Some((retry_of_task_id, reason)) = active_retry {
    abort_task(coordinator, retry_of_task_id, reason)?;
  }
  let task = coordinator.store.write(|tx| {
    task::create(
      tx,
      &text,
      predicted_files,
      predicted_lines,
      retry_of_task_id,
      predicted_file_list,
    )
  })?;
  println!("{}", task.id());
  Ok(())
}

pub(super) fn cmd_dispatch(
  coordinator: &Coordinator,
  task_id: i64,
  implementer: &str,
  reason: Option<&str>,
) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: task {task_id} is not in state drafted");
  };
  if task.state() != TaskState::Drafted {
    bail!("supervisor: task {task_id} is not in state drafted");
  }
  let flying = coordinator
    .store
    .read(task::all)?
    .into_iter()
    .find(|task| matches!(task.state(), TaskState::Dispatched | TaskState::InFlight));
  if let Some(flying) = flying {
    bail!(
      "supervisor: an implementer is already in flight ({} is in flight on task {})",
      session_name(coordinator, flying.session_id())?,
      flying.id()
    );
  }
  let Some(session) = coordinator
    .store
    .read(|tx| session::latest_named(tx, implementer))?
  else {
    bail!("supervisor: no session {implementer}; launch it first");
  };
  if !session.can_take_task() {
    if session.is_live() {
      bail!(
        "supervisor: {implementer} is the {}, not an implementer; only implementers take tasks",
        session.role()
      );
    }
    bail!("supervisor: {implementer} is stopped; launch it again first");
  }
  if let Some(prior) = last_task_on(coordinator, session.id())? {
    bail!(
      "supervisor: {implementer} already took task {} ({}); every task gets a fresh implementer",
      prior.id(),
      prior.state()
    );
  }

  let preamble = files_changed_since_launch(coordinator, &session)?;

  let prompt = format!(
    "{}{text}\n\n{CONTRACT}",
    preamble,
    text = task.text().trim_end()
  );
  // The task is measured from where the transcript and the branch stood
  // before the send: an agent may be at work, even past its commit, before
  // its transcript shows the prompt.
  let transcript_offset = transcript_size(session_transcript(coordinator, &session)?.as_deref());
  let base_head = coordinator.repo.head()?;
  // The task is only dispatched once the prompt is taken, so a send that
  // never is leaves it drafted and dispatchable again.
  if let Err(error) = cmd_prompt(coordinator, implementer, &prompt, false, 300) {
    record_run_event(
      coordinator,
      RunEventKind::DispatchFailed,
      &format!("task {task_id} -> {implementer}: {error}"),
    )?;
    return Err(error);
  }
  coordinator.store.write(|tx| {
    task::dispatch(
      tx,
      task_id,
      session.id(),
      transcript_offset as i64,
      &base_head,
      reason,
    )?;
    run_event::create(
      tx,
      RunEventKind::Dispatch,
      &format!("task {task_id} -> {implementer}"),
    )?;
    Ok(())
  })?;
  println!(
    "task {task_id} dispatched to {implementer}; watch `state --task {task_id}` — it prints `{task_id} committed_unverified` when the commit lands"
  );
  Ok(())
}

/// The session may have read the tree before its task arrived; name what moved
/// since it started so it rereads that first. Empty when nothing has.
fn files_changed_since_launch(coordinator: &Coordinator, session: &Session) -> Result<String> {
  let Some(head) = session.launched_head() else {
    return Ok(String::new());
  };
  let files = coordinator.repo.files_changed(head, "HEAD")?;
  if files.is_empty() {
    return Ok(String::new());
  }
  Ok(format!(
    "These files changed since your session started; read them first: {}\n\n",
    files.join(", ")
  ))
}

pub(super) fn new_commit_for(
  coordinator: &Coordinator,
  shas: &[String],
  base_head: Option<&str>,
) -> Result<Option<String>> {
  match base_head {
    Some(base_head) => coordinator.repo.new_commit_among(shas, base_head),
    None => Ok(None),
  }
}

fn failures_in_lineage(coordinator: &Coordinator, task_id: i64) -> Result<i64> {
  let mut failures = 0;
  let mut current = coordinator.store.read(|tx| task::get(tx, task_id))?;
  while let Some(task) = current {
    if task.state() == TaskState::Aborted {
      failures += 1;
    }
    current = match task.retry_of_task_id() {
      Some(retry_of_task_id) => coordinator
        .store
        .read(|tx| task::get(tx, retry_of_task_id))?,
      None => None,
    };
  }
  Ok(failures)
}

fn abort_task(coordinator: &Coordinator, task_id: i64, reason: &str) -> Result<(i64, String)> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: no task {task_id}");
  };
  if reason.trim().is_empty() {
    bail!("supervisor: abort requires a non-empty --reason");
  }
  if task.state().is_terminal() {
    bail!("supervisor: task {task_id} is already {}", task.state());
  }
  let dirty = coordinator.repo.status()?;
  coordinator.store.write(|tx| {
    task::abort(tx, task_id, reason)?;
    run_event::create(
      tx,
      RunEventKind::Aborted,
      &format!("task {task_id}: {reason}"),
    )?;
    Ok(())
  })?;
  if let Some(session_id) = task.session_id()
    && let Some(session) = coordinator.store.read(|tx| session::get(tx, session_id))?
    && session.is_live()
  {
    let detail = format!("task {task_id} -> {}", session.name());
    let outcome = coordinator.runtime.interrupt(session.name()).and_then(|()| {
      daemon_prompt(
        coordinator,
        session.name(),
        &format!(
          "supervisor: task {task_id} is aborted: {reason}. Stop, leave the tree clean, do not commit."
        ),
      )
      .then_some(())
      .with_context(|| format!("abort message did not reach {}", session.name()))
    });
    match outcome {
      Ok(()) => record_run_event(coordinator, RunEventKind::AbortInterrupt, &detail)?,
      Err(error) => record_run_event(
        coordinator,
        RunEventKind::AbortUnreachable,
        &format!("{detail}: {error}"),
      )?,
    }
  }
  let failures = failures_in_lineage(coordinator, task_id)?;
  Ok((failures, dirty))
}

pub(super) fn cmd_abort(coordinator: &Coordinator, task_id: i64, reason: &str) -> Result<()> {
  let (failures, dirty) = abort_task(coordinator, task_id, reason)?;
  let plural = if failures == 1 { "" } else { "s" };
  println!("task {task_id} aborted ({failures} abort{plural} on this task): {reason}");
  if !dirty.is_empty() {
    println!("WARNING: tree is dirty — the implementer did not leave it clean:\n{dirty}");
  }
  if failures >= 3 {
    println!("three aborts on the same task: escalate to the human");
  } else {
    println!(
      "adjust the task and retry with a fresh implementer: task new --retry-of {task_id} < task.md"
    );
  }
  Ok(())
}
