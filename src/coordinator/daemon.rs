//! The background poll: what the supervisor does on its own while the lead
//! works. Each poll reads every live session's transcript and acts on what
//! it finds by role: records the implementer's flight and commit, wakes and
//! compacts the commentator, and notes the lead crossing its stop threshold.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;

use super::{
  LEAD_STOP_TOKENS, daemon_prompt, new_commit_for, record_run_event, session_transcript, status_of,
};
use crate::domain::{AgentKind, ContextSize, Role, RunEventKind, Session, Task, TaskState};
use crate::infra::agent;
use crate::infra::session_runtime::SessionStatus;
use crate::infra::transcript_monitor::transcript_size;
use crate::persistence::{run as run_record, run_event, session, task};
use crate::run::Run;

/// The commentator is asked to compact once its context passes this.
const COMMENTATOR_COMPACT_TOKENS: u64 = 150_000;
/// A session whose transcript has not grown for this long while its runtime
/// reports it idle is kicked.
const STALE_SECONDS: f64 = 600.0;

pub(super) fn start(
  run: &Run,
  lead: &str,
  lead_session_id: &str,
  poll_interval: Duration,
) -> Result<()> {
  register_lead(run, lead, lead_session_id)?;
  run.store().write(|tx| {
    run_record::clear_stop_request(tx)?;
    run_event::create(
      tx,
      RunEventKind::DaemonStart,
      &format!("pid {}", std::process::id()),
    )?;
    Ok(())
  })?;
  let mut sizes: HashMap<String, u64> = HashMap::new();
  let mut missing_transcripts = HashSet::new();
  let mut compacting = false;
  loop {
    // One write transaction per poll: read the stop request, then stamp the poll.
    let stopping = run.store().write(|tx| {
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
    for session in run
      .store()
      .read(session::all)?
      .into_iter()
      .filter(Session::is_live)
    {
      let name = session.name();
      let Some(transcript) = session_transcript(run, &session)? else {
        if missing_transcripts.insert(name.to_owned()) {
          let danger = if session.role() == Role::Lead {
            "; the lead context stop threshold cannot fire"
          } else {
            ""
          };
          let detail = format!("{name} ({}): transcript not found{danger}", session.role());
          eprintln!("WARNING: {detail}");
          record_run_event(run, RunEventKind::TranscriptMissing, &detail)?;
        }
        continue;
      };
      if missing_transcripts.remove(name) {
        eprintln!(
          "supervisor: transcript found for {name}: {}",
          transcript.display()
        );
        record_run_event(
          run,
          RunEventKind::TranscriptFound,
          &format!("{name}: {}", transcript.display()),
        )?;
      }
      let size = transcript_size(Some(&transcript));
      let context = agent::for_session(&session).context_size(&transcript);
      let grew = sizes.get(name).copied() != Some(size);
      sizes.insert(name.to_owned(), size);
      run
        .store()
        .write(|tx| session::record_reading(tx, session.id(), context, grew, timestamp))?;
      let quiet = session.quiet_seconds(timestamp) as f64;

      match session.role() {
        Role::Implementer => {
          observe_implementer(run, &session, &transcript, quiet)?;
        }
        Role::Commentator => {
          observe_commentator(
            run,
            &session,
            Reading {
              transcript: &transcript,
              context,
              quiet,
            },
            &mut compacting,
          )?;
        }
        Role::Lead => observe_lead(run, &session, context)?,
      }
    }
    thread::sleep(poll_interval);
  }
  record_run_event(
    run,
    RunEventKind::DaemonExit,
    &format!("pid {}", std::process::id()),
  )
}

/// The lead is started by the human in Claude Code, so the daemon registers
/// it from what the lead says about itself. The same session id keeps its row
/// across daemon restarts; a different one is a new incarnation and stops the
/// old row.
fn register_lead(run: &Run, lead: &str, lead_session_id: &str) -> Result<()> {
  run.store().write(|tx| {
    let current = session::latest_named(tx, lead)?;
    if !current.is_some_and(|session| {
      session.is_live()
        && session.role() == Role::Lead
        && session.external_session_id() == lead_session_id
    }) {
      session::stop_named(tx, lead)?;
      session::create(
        tx,
        lead,
        Role::Lead,
        AgentKind::Claude,
        lead_session_id,
        None,
      )?;
    }
    Ok(())
  })?;
  Ok(())
}

/// Nudge a session that has gone quiet while its runtime reports it idle. The
/// kick is latched on the session so it happens once per stall.
fn kick_if_stalled(run: &Run, session: &Session, quiet: f64) -> Result<()> {
  if quiet > STALE_SECONDS
    && session.can_be_kicked()
    && status_of(run.runtime(), session.name()) == Some(SessionStatus::Idle)
    && daemon_prompt(run, session.name(), "continue")
  {
    run.store().write(|tx| {
      session::record_kick(tx, session.id())?;
      run_event::create(tx, RunEventKind::Kick, session.name())?;
      Ok(())
    })?;
  }
  Ok(())
}

fn observe_implementer(run: &Run, session: &Session, transcript: &Path, quiet: f64) -> Result<()> {
  let agent = agent::for_session(session);
  let task = run
    .store()
    .read(|tx| task::tasks_for_session(tx, session.id()))?
    .into_iter()
    .rev()
    .find(|task| matches!(task.state(), TaskState::Dispatched | TaskState::InFlight));
  let Some(task) = task else {
    return Ok(());
  };
  if task.state() == TaskState::Dispatched {
    let dispatch_offset = task.transcript_offset() as u64;
    if transcript_size(Some(transcript)) <= dispatch_offset {
      return Ok(());
    }
    let context = agent.context_before(transcript, dispatch_offset);
    run
      .store()
      .write(|tx| task::take_flight(tx, task.id(), context))?;
    return Ok(());
  }
  let head = run.repo().head()?;
  let shas = agent.commit_candidates(transcript, task.transcript_offset() as u64, &head);
  if let Some(sha) = new_commit_for(run, &shas, task.base_head())? {
    run.store().write(|tx| {
      task::record_commit(tx, task.id(), &sha, None)?;
      run_event::create(
        tx,
        RunEventKind::Committed,
        &format!("task {} {sha}", task.id()),
      )?;
      Ok(())
    })?;
  } else {
    kick_if_stalled(run, session, quiet)?;
  }
  Ok(())
}

/// What one daemon poll saw of a session's transcript.
struct Reading<'a> {
  transcript: &'a Path,
  context: ContextSize,
  quiet: f64,
}

fn observe_commentator(
  run: &Run,
  session: &Session,
  reading: Reading<'_>,
  compacting: &mut bool,
) -> Result<()> {
  let Reading {
    transcript,
    context,
    quiet,
  } = reading;
  let pending = run
    .store()
    .read(task::all)?
    .into_iter()
    .filter(Task::awaits_commentary)
    .collect::<Vec<_>>();
  let agent = agent::for_session(session);
  for task in pending {
    let sha = task.commit_sha().unwrap_or_default();
    let abbreviation = sha.get(..7).unwrap_or(sha);
    if agent.output_mentions(transcript, abbreviation) {
      run.store().write(|tx| {
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
        session.name(),
        &format!(
          "supervisor: commit {sha} landed for task {}; review it from git",
          task.id()
        ),
      )
    {
      run.store().write(|tx| {
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
    if daemon_prompt(run, session.name(), agent.compact_prompt()) {
      *compacting = true;
      record_run_event(
        run,
        RunEventKind::Compact,
        &format!("{} at {context}", session.name()),
      )?;
    }
  } else if context.is_under(COMMENTATOR_COMPACT_TOKENS) {
    *compacting = false;
  }
  kick_if_stalled(run, session, quiet)
}

/// Records the lead crossing its stop threshold once per lead session. Nothing
/// is pushed at the lead: an unsolicited prompt mid-thought is a context switch
/// it did not choose. The warning printed after every lead-facing command
/// carries the same fact at the moment the lead is already reading output.
fn observe_lead(run: &Run, session: &Session, context: ContextSize) -> Result<()> {
  if context.exceeds(LEAD_STOP_TOKENS) && session.can_latch_over_limit() {
    run.store().write(|tx| {
      session::record_over_limit(tx, session.id())?;
      run_event::create(tx, RunEventKind::StopLead, &format!("context {context}"))?;
      Ok(())
    })?;
  }
  Ok(())
}
