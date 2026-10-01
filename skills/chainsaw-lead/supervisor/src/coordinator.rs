use std::time::Duration;

use anyhow::Result;
use chrono::Utc;

use crate::cli::{Command, TaskCommand};
use crate::domain::{Role, RunEventKind, SessionKind, Task, TaskEvent, TaskState};
use crate::persistence::store::Store;
use crate::persistence::{run as run_record, run_event, task};
use crate::run::Run;

mod accept;
mod calibrate;
mod daemon;
mod prompt;
mod review;
mod sessions;
mod state;
mod tasks;

use accept::{cmd_accept, cmd_task_record_commentary, cmd_task_record_commit};
use calibrate::cmd_calibrate;
use prompt::{cmd_prompt, daemon_prompt};
use review::{cmd_finding, cmd_observe, cmd_poll, cmd_resolutions, cmd_resolve};
use sessions::{
  cmd_context, cmd_launch, cmd_start_commentator, cmd_watch_transcripts, session_name,
  session_transcript, task_session,
};
use state::{cmd_human_wait, cmd_state, cmd_stop};
use tasks::{NewTaskOptions, cmd_abort, cmd_dispatch, cmd_task_new, new_commit_for, task_commits};

const LEAD_STOP_TOKENS: u64 = 250_000;
const LEAD_WARN_TOKENS: u64 = 200_000;
/// A commit the lead has not judged after this long is being missed.
const COMMIT_UNATTENDED_SECONDS: i64 = 300;
/// Task monitors read state every few seconds; this much silence while a task
/// is out means nothing is watching.
const STATE_UNREAD_SECONDS: i64 = 120;
/// The daemon stamps the run record's `daemon_seen_at` on every poll; silence
/// this long means no daemon is running, whether it never started, was
/// stopped, or died.
const DAEMON_SILENT_SECONDS: i64 = 30;

pub fn execute(run: &Run, store: &Store, command: Command) -> Result<()> {
  let lead_facing = is_lead_facing(&command);
  dispatch(run, store, command)?;
  if lead_facing {
    for warning in standing_warnings(run, store)? {
      eprintln!("WARNING: {warning}");
    }
  }
  Ok(())
}

/// Commands whose output the lead reads. The daemon and the commentator's
/// watch never return; the rest print a value the lead pipes somewhere.
fn is_lead_facing(command: &Command) -> bool {
  !matches!(
    command,
    Command::Daemon { .. }
      | Command::WatchTranscripts { .. }
      | Command::TranscriptsDir
      | Command::Context { .. }
      | Command::Stop
  )
}

/// Facts the lead must act on, printed after every lead-facing command so
/// they do not depend on the lead remembering the skill. Each one is measured
/// from the store, never inferred from what the lead said.
fn standing_warnings(run: &Run, store: &Store) -> Result<Vec<String>> {
  let mut warnings = Vec::new();
  let at = Utc::now();
  let timestamp = at.timestamp_millis();
  if let Some(lead) = store
    .read(|tx| run.sessions(tx))?
    .into_iter()
    .find(|session| session.role() == Role::Lead && session.is_live())
  {
    let context = lead.context();
    if context.exceeds(LEAD_STOP_TOKENS) {
      warnings.push(format!(
        "lead context {context} is past {LEAD_STOP_TOKENS}: stop the run per the skill's Stopping section"
      ));
    } else if context.exceeds(LEAD_WARN_TOKENS) {
      warnings.push(format!("lead context {context} of {LEAD_STOP_TOKENS}"));
    }
  }
  let (tasks, record) = store.read(|tx| Ok((task::all(tx)?, run_record::get(tx)?)))?;
  for task in &tasks {
    if task.state() != TaskState::CommittedUnverified {
      continue;
    }
    let since = task
      .events()
      .iter()
      .find(|event| event.state() == TaskState::CommittedUnverified)
      .map_or(timestamp, |event| event.created_at().timestamp_millis());
    let age = (timestamp - since) / 1000;
    if age > COMMIT_UNATTENDED_SECONDS {
      warnings.push(format!(
        "task {} committed_unverified for {}, not accepted or aborted",
        task.id(),
        duration_text(age)
      ));
    }
  }
  let out: Vec<i64> = tasks
    .iter()
    .filter(|task| matches!(task.state(), TaskState::Dispatched | TaskState::InFlight))
    .map(Task::id)
    .collect();
  if let Some(task_id) = out.first() {
    let unread = match record.seconds_since_state_read(at) {
      Some(age) if age <= STATE_UNREAD_SECONDS => None,
      Some(age) => Some(format!("no state read for {}", duration_text(age))),
      None => Some("state has never been read".to_owned()),
    };
    if let Some(unread) = unread {
      warnings.push(format!(
        "{unread} while task {task_id} is out: is a monitor armed on `state --task {task_id}`?"
      ));
    }
  }
  let absent = match record.seconds_since_daemon_seen(at) {
    Some(age) if age <= DAEMON_SILENT_SECONDS => None,
    Some(age) => Some(format!("no daemon has polled for {}", duration_text(age))),
    None => Some("no daemon has run for this run".to_owned()),
  };
  if let Some(absent) = absent {
    warnings.push(format!(
      "{absent}: nothing observes sessions until `daemon` is started"
    ));
  }
  Ok(warnings)
}

fn duration_text(seconds: i64) -> String {
  if seconds < 60 {
    format!("{seconds}s")
  } else {
    format!("{}m", seconds / 60)
  }
}

fn dispatch(run: &Run, store: &Store, command: Command) -> Result<()> {
  match command {
    Command::Daemon {
      lead,
      session_id,
      poll_interval_ms,
    } => daemon::start(
      run,
      store,
      &lead,
      &session_id,
      Duration::from_millis(poll_interval_ms),
    ),
    Command::StartCommentator { role_prompt } => cmd_start_commentator(run, store, &role_prompt),
    Command::Launch { name, prompt } => {
      cmd_launch(run, store, &name, SessionKind::Implementer, &prompt)
    }
    Command::Prompt {
      name,
      text,
      wait,
      timeout,
    } => cmd_prompt(run, store, &name, &text, wait, timeout),
    Command::Task { action } => match action {
      TaskCommand::New {
        files,
        predicted_files,
        predicted_lines,
        retry_of_task_id,
        reason,
      } => cmd_task_new(
        run,
        store,
        NewTaskOptions {
          predicted_files,
          predicted_lines,
          retry_of_task_id,
          files: files.as_deref(),
          reason: reason.as_deref(),
        },
      ),
      TaskCommand::RecordCommit {
        task,
        sha,
        force,
        reason,
      } => cmd_task_record_commit(run, store, task, &sha, force, reason.as_deref()),
      TaskCommand::RecordCommentary {
        task,
        force,
        reason,
      } => cmd_task_record_commentary(store, task, force, reason.as_deref()),
    },
    Command::Abort { task, reason } => cmd_abort(run, store, task, &reason),
    Command::Dispatch { task, to, reason } => {
      cmd_dispatch(run, store, task, &to, reason.as_deref())
    }
    Command::Accept {
      task,
      force,
      reason,
    } => cmd_accept(run, store, task, force, reason.as_deref()),
    Command::Calibrate { task } => cmd_calibrate(run, store, task),
    Command::Observe { task, text } => cmd_observe(store, task, &text),
    Command::Finding { task, description } => cmd_finding(store, task, &description),
    Command::Poll {
      after_observation,
      task,
    } => cmd_poll(store, after_observation, task),
    Command::Resolve {
      finding,
      verdict,
      fix_task_id,
      reason,
    } => cmd_resolve(store, finding, &verdict, fix_task_id, &reason),
    Command::Resolutions => cmd_resolutions(store),
    Command::State { task } => cmd_state(run, store, task),
    Command::TranscriptsDir => {
      println!("{}", run.transcripts_dir().display());
      Ok(())
    }
    Command::WatchTranscripts { interval_ms } => cmd_watch_transcripts(run, store, interval_ms),
    Command::Context { name } => cmd_context(run, store, name.as_deref()),
    Command::HumanWait { action } => cmd_human_wait(store, action),
    Command::Stop => cmd_stop(store),
  }
}

/// Journals one supervisor action that belongs to no other write.
fn record_run_event(store: &Store, kind: RunEventKind, detail: &str) -> Result<()> {
  store.write(|tx| run_event::create(tx, kind, detail))?;
  Ok(())
}

/// Millisecond timestamp of the newest event of `task` that `matches`.
fn last_event_at(task: &Task, matches: impl Fn(&TaskEvent) -> bool) -> Option<i64> {
  task
    .events()
    .iter()
    .rev()
    .find(|event| matches(event))
    .map(|event| event.created_at().timestamp_millis())
}
