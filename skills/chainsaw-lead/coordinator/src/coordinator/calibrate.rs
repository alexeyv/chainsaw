//! Measuring a finished task against its prediction: files and lines the
//! commit actually touched, wall time from dispatch to commit, and the
//! context its implementer spent on it.

use anyhow::{Result, bail};
use regex::Regex;

use super::{last_event_at, session_transcript, task_session};
use crate::domain::{ContextSize, Session, TaskState};
use crate::persistence::store::Store;
use crate::persistence::{calibration, task};
use crate::run::Run;

fn stat_number(text: &str, noun: &str) -> i64 {
  Regex::new(&format!(r"(\d+) {noun}s?"))
    .expect("valid stat regex")
    .captures(text)
    .and_then(|capture| capture[1].parse().ok())
    .unwrap_or_default()
}

pub(super) fn cmd_calibrate(run: &Run, store: &Store, task_id: i64) -> Result<()> {
  let Some(task) = store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: task {task_id} has no commit yet");
  };
  let Some(commit_sha) = task.commit_sha() else {
    bail!("supervisor: task {task_id} has no commit yet");
  };
  let stat = run.repo().shortstat(commit_sha)?;
  let actual_files = stat_number(&stat, "file");
  let actual_lines = stat_number(&stat, "insertion") + stat_number(&stat, "deletion");
  let dispatched_at = last_event_at(&task, |event| event.state() == TaskState::Dispatched);
  let committed_at = last_event_at(&task, |event| {
    event.state() == TaskState::CommittedUnverified
  });
  let wall = dispatched_at
    .zip(committed_at)
    .map(|(start, end)| (end - start) as f64 / 1000.0);
  let session = task_session(run, store, &task)?;
  let next_offset = match task.session_id() {
    Some(session_id) => store
      .read(|tx| task::tasks_for_session(tx, session_id))?
      .into_iter()
      .find(|candidate| candidate.id() > task_id && candidate.transcript_offset() > 0)
      .map(|candidate| candidate.transcript_offset() as u64),
    None => None,
  };
  let peak = match &session {
    Some(session) => session.agent().context_peak(
      session_transcript(session)?,
      task.transcript_offset() as u64,
      next_offset,
    ),
    None => ContextSize::UNKNOWN,
  };
  // No usage within the task's slice of the transcript falls back to the
  // session's recorded maximum.
  let recorded_max = session
    .as_ref()
    .map_or(ContextSize::UNKNOWN, Session::context_max);
  let end = if peak.exceeds(0) {
    peak
  } else {
    recorded_max.or(peak)
  };
  let base = task.context_size_start();
  let context = end.since(base);
  store.write(|tx| {
    calibration::create(
      tx,
      task_id,
      task.predicted_files(),
      task.predicted_lines(),
      actual_files,
      actual_lines,
      wall,
      base,
      end,
    )
  })?;
  let wall_text = wall.map_or_else(|| "None".to_owned(), |wall| (wall as i64).to_string());
  println!(
    "task {task_id}: predicted {} files/{} lines, actual {actual_files} files/{actual_lines} lines, wall {wall_text}s, context {context} (session {end}, base {base})",
    task.predicted_files(),
    task.predicted_lines(),
  );
  Ok(())
}
