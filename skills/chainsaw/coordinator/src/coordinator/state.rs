//! What the lead sees of the run: the state report with its task timelines,
//! session contexts, time summary and recent supervisor actions, and the
//! human-wait and stop commands that mark the run's own state.

use anyhow::{Context, Result};
use chrono::{Local, TimeZone, Utc};
use strum::IntoEnumIterator;

use super::{last_event_at, session_name};
use crate::cli::HumanWaitAction;
use crate::domain::{Role, RunEventKind, TaskState};
use crate::persistence::store::Store;
use crate::persistence::{human_wait, run as run_record, run_event, task};
use crate::run::Run;

/// An implementer past this much context is flagged in the report.
const IMPLEMENTER_LIMIT_TOKENS: u64 = 100_000;

fn short_sha(sha: &str) -> &str {
  sha.get(..10).unwrap_or(sha)
}

pub(super) fn cmd_state(run: &Run, store: &Store, only_task: Option<i64>) -> Result<()> {
  store.write(run_record::record_state_read)?;
  if let Some(task_id) = only_task {
    let task = store
      .read(|tx| task::get(tx, task_id))?
      .with_context(|| format!("supervisor: no task {task_id}"))?;
    println!("{task_id} {}", task.state());
    return Ok(());
  }
  println!("tasks");
  let tasks = store.read(task::all)?;
  for task in tasks {
    let mut timeline = TaskState::iter()
      .filter_map(|state| {
        last_event_at(&task, |event| event.state() == state)
          .map(|at| format!("{state}@{}", clock_time(at)))
      })
      .collect::<Vec<_>>();
    if let Some(delivered_at) = task.commentary_delivered_at() {
      timeline.push(format!(
        "commentary-delivered@{}",
        clock_time(delivered_at.timestamp_millis())
      ));
    }
    let timeline = timeline.join(" ");
    let retry = task
      .retry_of_task_id()
      .map_or_else(String::new, |id| format!("  retry of {id}"));
    let reason = task
      .reason()
      .map_or_else(String::new, |reason| format!("  reason: {reason}"));
    println!(
      "  {:>3} {:<10} {:<16} {:<10} {timeline}{retry}{reason}",
      task.id(),
      task.state(),
      session_name(run, store, task.session_id())?,
      task.commit_sha().map(short_sha).unwrap_or("-")
    );
  }
  println!("sessions");
  for session in store.read(|tx| run.sessions(tx))? {
    let mut flags = String::new();
    let implementer = session.role() == Role::Implementer;
    if implementer && session.context().exceeds(IMPLEMENTER_LIMIT_TOKENS) {
      flags.push_str(" OVER-LIMIT");
    }
    let quiet = session.quiet_seconds(Utc::now());
    // A transcript gone from under the run fails the report, not reads as zero.
    session.transcript()?;
    println!(
      "  {:<16} {:<12} context {:>7} (max {}) quiet {quiet}s{flags}",
      session.name(),
      session.role(),
      session.context(),
      session.context_max()
    );
  }
  print_time_summary(store)?;
  if store.read(human_wait::open)?.is_some() {
    println!("  (a human wait is open)");
  }
  let events = store.read(|tx| run_event::recent(tx, STATE_EVENT_KINDS, 5))?;
  for event in events {
    println!(
      "  {} {} {}",
      clock_time(event.created_at().timestamp_millis()),
      event.kind(),
      event.detail()
    );
  }
  Ok(())
}

/// The supervisor actions worth a line at the bottom of `state`: prompts it
/// pushed at a session or gave up on, the overrides the lead forced, and how
/// an abort reached its session. Launches, dispatches, commits and the
/// daemon's own lifecycle are visible elsewhere in `state` and stay out.
const STATE_EVENT_KINDS: &[RunEventKind] = &[
  RunEventKind::StopLead,
  RunEventKind::Kick,
  RunEventKind::Compact,
  RunEventKind::PromptQueued,
  RunEventKind::PromptTaken,
  RunEventKind::PromptFailed,
  RunEventKind::Accepted,
  RunEventKind::ForcedCommit,
  RunEventKind::ForcedCommentary,
  RunEventKind::CommentaryWake,
  RunEventKind::AbortInterrupt,
  RunEventKind::AbortUnreachable,
];

fn clock_time(millis: i64) -> String {
  Local.timestamp_millis_opt(millis).single().map_or_else(
    || "-".to_owned(),
    |time| time.format("%H:%M:%S").to_string(),
  )
}

fn print_time_summary(store: &Store) -> Result<()> {
  let tasks = store.read(task::all)?;
  let first = tasks
    .iter()
    .flat_map(|task| task.events())
    .map(|event| event.created_at().timestamp_millis())
    .min();
  let mut busy = 0;
  for task in &tasks {
    let start = last_event_at(task, |event| event.state() == TaskState::Dispatched);
    let end = task
      .events()
      .iter()
      .find(|event| {
        matches!(
          event.state(),
          TaskState::CommittedUnverified | TaskState::Accepted | TaskState::Aborted
        )
      })
      .map(|event| event.created_at().timestamp_millis());
    if let Some(start) = start {
      busy += end.unwrap_or_else(|| Utc::now().timestamp_millis()) - start;
    }
  }
  let at = Utc::now();
  let human: i64 = store
    .read(human_wait::all)?
    .iter()
    .map(|wait| wait.duration(at).num_milliseconds())
    .sum();
  if let Some(first) = first {
    let wall = Utc::now().timestamp_millis() - first;
    let percentage = if wall == 0 {
      0.0
    } else {
      100.0 * busy as f64 / wall as f64
    };
    println!(
      "time  wall {}s  implementer-busy {}s ({percentage:.0}%)  waiting-on-human {}s",
      wall / 1000,
      busy / 1000,
      human / 1000
    );
  }
  Ok(())
}

pub(super) fn cmd_human_wait(store: &Store, action: HumanWaitAction) -> Result<()> {
  match action {
    HumanWaitAction::Start => {
      store.write(human_wait::start)?;
    }
    HumanWaitAction::End => {
      store.write(human_wait::end)?;
    }
  }
  Ok(())
}

pub(super) fn cmd_stop(store: &Store) -> Result<()> {
  store.write(|tx| {
    run_record::request_stop(tx)?;
    run_event::create(tx, RunEventKind::Stop, "run ended by the lead")?;
    Ok(())
  })?;
  println!("supervisor: stopped; the daemon will exit on its next poll");
  Ok(())
}
