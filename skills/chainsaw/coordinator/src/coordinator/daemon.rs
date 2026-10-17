//! The background poll: what the supervisor does on its own while the lead
//! works. Each poll reads every live session's transcript and acts on what
//! it finds by role: records the implementer's flight and commit, wakes the
//! commentator on commits and keeps its context in hand, and notes the lead
//! crossing its stop threshold.

use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use super::sessions::{commentator_recovery_prompt, start_commentator};
use super::{LEAD_STOP_TOKENS, daemon_prompt, new_commit_for, record_run_event};
use crate::domain::{
  AgentKind, COMMENTATOR_COMPACT_TOKENS, CommentatorAction, CommentatorReading, CommentatorWatch,
  ContextSize, Role, RunEventKind, Session, SessionStatus, Task, TaskState, Transcript,
};

use crate::persistence::store::Store;
use crate::persistence::{run as run_record, run_event, session, task};
use crate::run::Run;

/// A session whose transcript has not grown for this long while its runtime
/// reports it idle is kicked.
const STALE_SECONDS: f64 = 600.0;

pub(super) fn start(
  run: &Run,
  store: &Store,
  lead: &str,
  lead_session_id: &str,
  lead_agent: AgentKind,
  poll_interval: Duration,
) -> Result<()> {
  register_lead(run, store, lead, lead_session_id, lead_agent)?;
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
  let mut watches: HashMap<i64, CommentatorWatch> = HashMap::new();
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
    let sessions: Vec<Session<'_>> = store
      .read(|tx| run.sessions(tx))?
      .into_iter()
      .filter(Session::is_live)
      .collect();
    watches.retain(|id, _| sessions.iter().any(|session| session.id() == *id));
    for session in sessions {
      let name = session.name();
      let transcript = session.transcript();
      let size = transcript.size()?;
      let context = transcript.context_size()?;
      let grew = sizes.get(name).copied() != Some(size);
      sizes.insert(name.to_owned(), size);
      store.write(|tx| run.record_session_reading(tx, session.id(), context, grew, timestamp))?;
      let quiet = session.quiet_seconds(timestamp) as f64;

      match session.role() {
        Role::Implementer => {
          observe_implementer(run, store, &session, transcript, quiet)?;
        }
        Role::Commentator => {
          // The session was read before this poll's reading was recorded, so
          // its context is the one before.
          let watch = watches
            .entry(session.id())
            .or_insert_with(|| CommentatorWatch::new(compacts_itself(&session), session.context()));
          observe_commentator(
            run,
            store,
            &session,
            CommentatorReading {
              context,
              reviews: reviews_since(store, session.started_at())?,
              age_seconds: (timestamp - session.started_at()).num_seconds(),
            },
            quiet,
            watch,
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

/// The lead is started by the human on an agent of their choosing, so the
/// daemon registers it from what the lead says about itself, and with the
/// transcript that agent is already writing for it. The same session id
/// keeps its row across daemon restarts; a different one, as a relaunch on
/// any agent has, is a new incarnation and stops the old row.
fn register_lead(
  run: &Run,
  store: &Store,
  lead: &str,
  lead_session_id: &str,
  lead_agent: AgentKind,
) -> Result<()> {
  let Some(transcript) = run.agent(lead_agent).transcript(run.dir(), lead_session_id) else {
    bail!(
      "supervisor: lead session {lead_session_id} has no {lead_agent} transcript; check --session-id and --agent"
    );
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
        lead_agent,
        lead_session_id,
        None,
        &transcript,
        None,
        None,
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
    if transcript.size()? <= dispatch_offset {
      return Ok(());
    }
    let context = transcript.context_before(dispatch_offset)?;
    store.write(|tx| task::take_flight(tx, task.id(), context))?;
    return Ok(());
  }
  let head = run.repo().head()?;
  let shas = transcript.commit_candidates(task.transcript_offset() as u64, &head)?;
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

/// Wakes the commentator on each commit waiting for its review, unless the
/// watch holds commits back, and does what the watch says about its context.
/// A commentator is nudged only while a commit waits for it: idle with
/// nothing to review is where it should be.
fn observe_commentator(
  run: &Run,
  store: &Store,
  session: &Session,
  reading: CommentatorReading,
  quiet: f64,
  watch: &mut CommentatorWatch,
) -> Result<()> {
  let waiting = awaiting_commentary(store)?;
  let name = session.name();
  match watch.poll(reading, || session.status() == Some(SessionStatus::Idle)) {
    CommentatorAction::Wait => {
      if !watch.holds_commits() && !waiting.is_empty() {
        wake_on_commits(run, store, session, &waiting)?;
        kick_if_stalled(run, store, session, quiet)?;
      }
    }
    CommentatorAction::Recover => {
      let prompt = commentator_recovery_prompt(run, session.role_prompt(), &waiting);
      if daemon_prompt(run, store, name, &prompt) {
        record_requests(store, &waiting)?;
        record_run_event(
          store,
          RunEventKind::CommentatorRecover,
          &format!("{name} at {}", reading.context),
        )?;
      } else {
        watch.owe_recovery();
      }
    }
    CommentatorAction::AskToStop => {
      let reason = if compacts_itself(session) {
        format!("your context is past {COMMENTATOR_COMPACT_TOKENS} tokens and has not compacted")
      } else {
        "you are due for a fresh session".to_owned()
      };
      daemon_prompt(
        run,
        store,
        name,
        &format!(
          "supervisor: {reason}. Finish the review you are on, write your notes, and stop; take up nothing new. The supervisor will bring you back."
        ),
      );
      record_run_event(
        store,
        RunEventKind::CommentatorStop,
        &format!("{name} at {}", reading.context),
      )?;
    }
    CommentatorAction::Interrupt => {
      let detail = match session.interrupt() {
        Ok(()) => name.to_owned(),
        Err(error) => format!("{name}: {error}"),
      };
      record_run_event(store, RunEventKind::CommentatorInterrupt, &detail)?;
    }
    CommentatorAction::Compact => {
      if daemon_prompt(run, store, name, session.agent().compact_prompt()) {
        record_run_event(
          store,
          RunEventKind::Compact,
          &format!("{name} at {}", reading.context),
        )?;
      }
    }
    CommentatorAction::Restart => {
      restart_commentator(run, store, session, reading.context, &waiting)?;
    }
  }
  Ok(())
}

/// Whether the commentator's agent was told at launch to compact itself.
fn compacts_itself(session: &Session) -> bool {
  session
    .agent()
    .compact_at_args(COMMENTATOR_COMPACT_TOKENS)
    .is_some()
}

/// Reviews delivered since `started_at`: by the commentator started then.
fn reviews_since(store: &Store, started_at: DateTime<Utc>) -> Result<u32> {
  let reviews = store
    .read(task::all)?
    .iter()
    .filter(|task| {
      task
        .commentary_delivered_at()
        .is_some_and(|delivered| delivered >= started_at)
    })
    .count();
  Ok(u32::try_from(reviews).unwrap_or(u32::MAX))
}

/// The tasks whose commits wait for the commentator's review: committed,
/// with no observation or finding from it on the task since.
fn awaiting_commentary(store: &Store) -> Result<Vec<Task>> {
  Ok(
    store
      .read(task::all)?
      .into_iter()
      .filter(Task::awaits_commentary)
      .collect(),
  )
}

/// Wakes the commentator once on each waiting commit it has not been woken on.
fn wake_on_commits(run: &Run, store: &Store, session: &Session, waiting: &[Task]) -> Result<()> {
  for task in waiting {
    if task.commentary_requested_at().is_some() {
      continue;
    }
    let sha = task.commit_sha().unwrap_or_default();
    if daemon_prompt(
      run,
      store,
      session.name(),
      &format!(
        "supervisor: commit {sha} landed for task {}; review it from git",
        task.id()
      ),
    ) {
      record_requests(store, std::slice::from_ref(task))?;
    }
  }
  Ok(())
}

/// Stamps that commentary was requested on each task's commit, journaling the
/// first request of each.
fn record_requests(store: &Store, tasks: &[Task]) -> Result<()> {
  store.write(|tx| {
    for task in tasks {
      if task::record_commentary_request(tx, task.id())? {
        run_event::create(
          tx,
          RunEventKind::CommentaryWake,
          &format!(
            "task {} {}",
            task.id(),
            task.commit_sha().unwrap_or_default()
          ),
        )?;
      }
    }
    Ok(())
  })
}

/// Replaces the commentator with a fresh one on the same role prompt, which
/// resumes from the same notes and is told which commits wait for it. Its
/// pane is closed first. A replacement that fails is journaled and tried
/// again later.
fn restart_commentator(
  run: &Run,
  store: &Store,
  session: &Session,
  context: ContextSize,
  waiting: &[Task],
) -> Result<()> {
  let name = session.name();
  let Some(role_prompt) = session.role_prompt() else {
    return record_run_event(
      store,
      RunEventKind::CommentatorRestartFailed,
      &format!("{name}: it was started without a role prompt"),
    );
  };
  if let Some(pane_id) = session.pane_id() {
    // A pane already gone is what a close is for.
    let _ = run.runtime().abandon(name, pane_id);
  }
  match start_commentator(run, store, role_prompt, waiting) {
    Ok(_) => {
      record_requests(store, waiting)?;
      record_run_event(
        store,
        RunEventKind::CommentatorRestart,
        &format!("{name} at {context}"),
      )
    }
    Err(error) => record_run_event(
      store,
      RunEventKind::CommentatorRestartFailed,
      &format!("{name}: {error}"),
    ),
  }
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
