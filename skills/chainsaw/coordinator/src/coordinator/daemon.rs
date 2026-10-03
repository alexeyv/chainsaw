//! The background poll: what the supervisor does on its own while the lead
//! works. Each poll reads every live session's transcript and acts on what
//! it finds by role: records the implementer's flight and commit, wakes and
//! compacts the commentator, and notes the lead crossing its stop threshold.

use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::Utc;

use super::{LEAD_STOP_TOKENS, daemon_prompt, new_commit_for, record_run_event};
use crate::domain::{
  AgentKind, ContextSize, Role, RunEventKind, Session, SessionStatus, Task, TaskState, Transcript,
};

use crate::persistence::store::Store;
use crate::persistence::{run as run_record, run_event, session, task};
use crate::run::Run;

/// The commentator is asked to compact once its context passes this.
const COMMENTATOR_COMPACT_TOKENS: u64 = 150_000;
/// A session whose transcript has not grown for this long while its runtime
/// reports it idle is kicked.
const STALE_SECONDS: f64 = 600.0;

pub(super) fn start(
  run: &Run,
  store: &Store,
  lead: &str,
  lead_session_id: &str,
  poll_interval: Duration,
) -> Result<()> {
  register_lead(run, store, lead, lead_session_id)?;
  store.write(|tx| {
    run_record::clear_stop_request(tx)?;
    run_event::create(
      tx,
      RunEventKind::DaemonStart,
      &format!("pid {}", std::process::id()),
    )?;
    Ok(())
  })?;
  let mut sizes: HashMap<String, u64> = HashMap::new();
  let mut compacting = false;
  loop {
    // One write transaction per poll: read the stop request, then stamp the poll.
    let stopping = store.write(|tx| {
      let stopping = run_record::get(tx)?.is_stopping();
      if !stopping {
        run_record::record_daemon_seen(tx)?;
      }
      Ok(stopping)
    })?;
    if stopping {
      break;
    }
    let timestamp = Utc::now();
    for session in store
      .read(|tx| run.sessions(tx))?
      .into_iter()
      .filter(Session::is_live)
    {
      let name = session.name();
      let transcript = session.transcript()?;
      let size = transcript.size();
      let context = transcript.context_size();
      let grew = sizes.get(name).copied() != Some(size);
      sizes.insert(name.to_owned(), size);
      store.write(|tx| run.record_session_reading(tx, session.id(), context, grew, timestamp))?;
      let quiet = session.quiet_seconds(timestamp) as f64;

      match session.role() {
        Role::Implementer => {
          observe_implementer(run, store, &session, &*transcript, quiet)?;
        }
        Role::Commentator => {
          observe_commentator(
            run,
            store,
            &session,
            &*transcript,
            context,
            quiet,
            &mut compacting,
          )?;
        }
        Role::Lead => observe_lead(run, store, &session, context)?,
      }
    }
    thread::sleep(poll_interval);
  }
  record_run_event(
    store,
    RunEventKind::DaemonExit,
    &format!("pid {}", std::process::id()),
  )
}

/// The lead is started by the human in Claude Code, so the daemon registers
/// it from what the lead says about itself, and with the transcript it is
/// already writing. The same session id keeps its row across daemon restarts;
/// a different one is a new incarnation and stops the old row.
fn register_lead(run: &Run, store: &Store, lead: &str, lead_session_id: &str) -> Result<()> {
  let Some(transcript) = run
    .agent(AgentKind::Claude)
    .transcript(run.dir(), lead_session_id)
  else {
    bail!("supervisor: lead session {lead_session_id} has no transcript; check --session-id");
  };
  store.write(|tx| {
    let current = run.session_named(tx, lead)?;
    if !current.is_some_and(|session| {
      session.is_live()
        && session.role() == Role::Lead
        && session.external_session_id() == lead_session_id
    }) {
      session::stop_named(tx, lead)?;
      run.register_session(
        tx,
        lead,
        Role::Lead,
        AgentKind::Claude,
        lead_session_id,
        None,
        &transcript,
      )?;
    }
    Ok(())
  })?;
  Ok(())
}

/// Nudge a session that has gone quiet while its runtime reports it idle. The
/// kick is latched on the session so it happens once per stall.
fn kick_if_stalled(run: &Run, store: &Store, session: &Session, quiet: f64) -> Result<()> {
  if quiet > STALE_SECONDS
    && session.can_be_kicked()
    && session.status() == Some(SessionStatus::Idle)
    && daemon_prompt(run, store, session.name(), "continue")
  {
    store.write(|tx| {
      run.record_session_kick(tx, session.id())?;
      run_event::create(tx, RunEventKind::Kick, session.name())?;
      Ok(())
    })?;
  }
  Ok(())
}

fn observe_implementer(
  run: &Run,
  store: &Store,
  session: &Session,
  transcript: &dyn Transcript,
  quiet: f64,
) -> Result<()> {
  let task = store
    .read(|tx| task::tasks_for_session(tx, session.id()))?
    .into_iter()
    .rev()
    .find(|task| matches!(task.state(), TaskState::Dispatched | TaskState::InFlight));
  let Some(task) = task else {
    return Ok(());
  };
  if task.state() == TaskState::Dispatched {
    let dispatch_offset = task.transcript_offset() as u64;
    if transcript.size() <= dispatch_offset {
      return Ok(());
    }
    let context = transcript.context_before(dispatch_offset);
    store.write(|tx| task::take_flight(tx, task.id(), context))?;
    return Ok(());
  }
  let head = run.repo().head()?;
  let shas = transcript.commit_candidates(task.transcript_offset() as u64, &head);
  if let Some(sha) = new_commit_for(run, &shas, task.base_head())? {
    store.write(|tx| {
      task::record_commit(tx, task.id(), &sha, None)?;
      run_event::create(
        tx,
        RunEventKind::Committed,
        &format!("task {} {sha}", task.id()),
      )?;
      Ok(())
    })?;
  } else {
    kick_if_stalled(run, store, session, quiet)?;
  }
  Ok(())
}

fn observe_commentator(
  run: &Run,
  store: &Store,
  session: &Session,
  transcript: &dyn Transcript,
  context: ContextSize,
  quiet: f64,
  compacting: &mut bool,
) -> Result<()> {
  let pending = store
    .read(task::all)?
    .into_iter()
    .filter(Task::awaits_commentary)
    .collect::<Vec<_>>();
  for task in pending {
    let sha = task.commit_sha().unwrap_or_default();
    let abbreviation = sha.get(..7).unwrap_or(sha);
    if transcript.output_mentions(abbreviation) {
      store.write(|tx| {
        if task::record_commentary_delivery(tx, task.id())? {
          run_event::create(
            tx,
            RunEventKind::CommentaryDelivered,
            &format!("task {}", task.id()),
          )?;
        }
        Ok(())
      })?;
    } else if task.commentary_requested_at().is_none()
      && daemon_prompt(
        run,
        store,
        session.name(),
        &format!(
          "supervisor: commit {sha} landed for task {}; review it from git",
          task.id()
        ),
      )
    {
      store.write(|tx| {
        if task::record_commentary_request(tx, task.id())? {
          run_event::create(
            tx,
            RunEventKind::CommentaryWake,
            &format!("task {} {sha}", task.id()),
          )?;
        }
        Ok(())
      })?;
    }
  }
  if context.exceeds(COMMENTATOR_COMPACT_TOKENS) && !*compacting {
    if daemon_prompt(run, store, session.name(), session.agent().compact_prompt()) {
      *compacting = true;
      record_run_event(
        store,
        RunEventKind::Compact,
        &format!("{} at {context}", session.name()),
      )?;
    }
  } else if context.is_under(COMMENTATOR_COMPACT_TOKENS) {
    *compacting = false;
  }
  kick_if_stalled(run, store, session, quiet)
}

/// Records the lead crossing its stop threshold once per lead session. Nothing
/// is pushed at the lead: an unsolicited prompt mid-thought is a context switch
/// it did not choose. The warning printed after every lead-facing command
/// carries the same fact at the moment the lead is already reading output.
fn observe_lead(run: &Run, store: &Store, session: &Session, context: ContextSize) -> Result<()> {
  if context.exceeds(LEAD_STOP_TOKENS) && session.can_latch_over_limit() {
    store.write(|tx| {
      run.record_session_over_limit(tx, session.id())?;
      run_event::create(tx, RunEventKind::StopLead, &format!("context {context}"))?;
      Ok(())
    })?;
  }
  Ok(())
}
