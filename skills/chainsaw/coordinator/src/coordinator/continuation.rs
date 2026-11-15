//! A run outlives one lead's context. The lead that stops hands off a
//! continuation prompt; the next lead started in the same run directory reads
//! it back with the run's own facts, and takes the run over when its daemon
//! starts. Starting over instead is a deliberate reset that archives the
//! run's state.

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::Utc;

use super::DAEMON_SILENT_SECONDS;
use crate::domain::RunEventKind;
use crate::persistence::store::Store;
use crate::persistence::{finding, run as run_record, run_event, task};
use crate::run::Run;

/// Where the continuation prompt is kept, in the run's state directory
/// beside the database.
const CONTINUATION_FILE_NAME: &str = "chainsaw-continuation.md";

fn continuation_path(run: &Run) -> PathBuf {
  run.state_dir().join(CONTINUATION_FILE_NAME)
}

/// Saves the continuation prompt read from standard input, replacing any
/// earlier one.
pub(super) fn cmd_handoff(run: &Run, store: &Store) -> Result<()> {
  let mut text = String::new();
  std::io::stdin().read_to_string(&mut text)?;
  if text.trim().is_empty() {
    bail!("supervisor: the continuation prompt on standard input is empty");
  }
  let path = continuation_path(run);
  fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
  store.write(|tx| run_event::create(tx, RunEventKind::Handoff, &path.to_string_lossy()))?;
  println!(
    "supervisor: continuation prompt saved to {}; the next lead started in {} continues from it",
    path.display(),
    run.dir().display()
  );
  Ok(())
}

/// Tells a lead starting up whether it begins a new run or continues one,
/// and for a continuation, the run's own facts and the prompt the lead
/// before it handed off.
pub(super) fn cmd_continuation(run: &Run, store: &Store) -> Result<()> {
  let (tasks, unresolved, record, sessions) = store.read(|tx| {
    Ok((
      task::all(tx)?,
      finding::unresolved(tx, None)?,
      run_record::get(tx)?,
      run.sessions(tx)?.len(),
    ))
  })?;
  if tasks.is_empty() && sessions == 0 {
    println!("new run: no tasks and no sessions yet");
    return Ok(());
  }
  let cursor = record.observation_cursor();
  println!(
    "continuation: {} tasks, {} unresolved findings, observation cursor {cursor}",
    tasks.len(),
    unresolved.len()
  );
  println!("next poll: poll --after-observation {cursor}");
  println!();
  let path = continuation_path(run);
  match fs::read_to_string(&path) {
    Ok(text) => {
      println!(
        "continuation prompt from the previous lead ({}):",
        path.display()
      );
      print!("{text}");
      if !text.ends_with('\n') {
        println!();
      }
    }
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => println!(
      "no continuation prompt was handed off; rebuild what you need from `state`, `poll` and git"
    ),
    Err(error) => {
      return Err(error).with_context(|| format!("cannot read {}", path.display()));
    }
  }
  Ok(())
}

/// Archives the run's state directory so that the next command starts a new
/// run, refusing while a daemon still polls it. Nothing is deleted, and the
/// panes of the run's sessions are left as they are.
pub(super) fn cmd_reset(run: &Run, store: &Store) -> Result<()> {
  let (tasks, sessions, record) = store.read(|tx| {
    Ok((
      task::all(tx)?.len(),
      run.sessions(tx)?.len(),
      run_record::get(tx)?,
    ))
  })?;
  if tasks == 0 && sessions == 0 {
    println!("supervisor: nothing to reset; the run has no tasks and no sessions");
    return Ok(());
  }
  let polling = record
    .seconds_since_daemon_seen(Utc::now())
    .is_some_and(|age| age <= DAEMON_SILENT_SECONDS);
  if polling && !record.is_stopping() {
    bail!("supervisor: a daemon is still running this run; `stop` it before resetting");
  }
  let state_dir = run.state_dir();
  let archive = PathBuf::from(format!(
    "{}.archived-{}",
    state_dir.display(),
    Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
  ));
  fs::rename(state_dir, &archive).with_context(|| {
    format!(
      "cannot archive {} to {}",
      state_dir.display(),
      archive.display()
    )
  })?;
  println!(
    "supervisor: archived this run's state to {}; the next command in {} starts a new run",
    archive.display(),
    run.dir().display()
  );
  Ok(())
}
