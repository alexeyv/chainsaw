//! The background poll: what the supervisor does on its own while the lead
//! works. Each poll reads every live session's transcript and acts on what
//! it finds by role: records the implementer's flight and commit, wakes and
//! compacts the commentator, and notes the lead crossing its stop threshold.

use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
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
/// Where a detached daemon's stderr goes, in the state directory.
const LOG_FILE_NAME: &str = "chainsaw-daemon.log";
/// How long `daemon --detach` waits for the daemon to register the lead and
/// read every transcript once.
const DETACH_TIMEOUT: Duration = Duration::from_secs(30);

/// Starts this same command again without `--detach`, as a process of its own
/// that outlives the terminal it was started from, and returns once that
/// daemon has registered the lead and read every transcript once.
pub(super) fn detach(run: &Run, store: &Store) -> Result<()> {
  let log_path = run.state_dir().join(LOG_FILE_NAME);
  let log = OpenOptions::new()
    .create(true)
    .append(true)
    .open(&log_path)
    .with_context(|| format!("cannot open {}", log_path.display()))?;
  let written_before = log.metadata()?.len() as usize;
  let mut command = Command::new(env::current_exe()?);
  command
    .args(env::args_os().skip(1).filter(|arg| arg != "--detach"))
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(log);
  detached(&mut command)?;
  // A pid can come back, so only a start newer than this one counts; another
  // daemon started alongside may have written its own start after ours.
  let earlier = store
    .read(|tx| run_event::recent(tx, &[RunEventKind::DaemonStart], 1))?
    .first()
    .map_or(0, |event| event.id());
  let mut child = command.spawn().context("cannot start the daemon")?;
  let started = format!("pid {}", child.id());
  let deadline = Instant::now() + DETACH_TIMEOUT;
  loop {
    // Exit first, so a daemon that recorded its start and has already died
    // since is not reported started. One that dies between the two checks
    // still is.
    if let Some(status) = child.try_wait()? {
      let exited = format!("supervisor: the daemon exited ({status}) before it started");
      // The exit is the news; what the daemon said is added only if readable.
      let said = fs::read(&log_path)
        .ok()
        .map(|log| {
          String::from_utf8_lossy(log.get(written_before..).unwrap_or_default()).into_owned()
        })
        .unwrap_or_default();
      let said = said.trim_end();
      bail!(
        "{}",
        if said.is_empty() {
          exited
        } else {
          format!("{said}\n{exited}")
        }
      );
    }
    if store
      .read(|tx| run_event::after(tx, RunEventKind::DaemonStart, earlier))?
      .iter()
      .any(|event| event.detail() == started)
    {
      println!("daemon started, {started}");
      return Ok(());
    }
    if Instant::now() > deadline {
      // Stopped, so that a retry cannot meet a late one still starting.
      let stopped = match child.kill() {
        Ok(()) => String::new(),
        Err(error) => format!(", and could not be stopped: {error}"),
      };
      bail!(
        "supervisor: the daemon ({started}) has not started after {}s{stopped}; see {}",
        DETACH_TIMEOUT.as_secs(),
        log_path.display()
      );
    }
    thread::sleep(Duration::from_millis(50));
  }
}

/// A console of its own with no window: closing the pane's console does not
/// reach the daemon, and the console programs it runs (herdr, git) share the
/// hidden one instead of each opening a window, as they would under
/// DETACHED_PROCESS.
///
/// std passes every inheritable handle to the child, and this process's own
/// standard handles are the caller's pipes, inherited. Left inheritable, the
/// daemon would hold them open and the caller would wait for the end of this
/// command's output for as long as the daemon runs. std has no safe way to
/// clear the flag, hence the one unsafe call in the crate.
// ponytail: the three standard handles only; another inheritable handle a
// caller leaks into this process still reaches the daemon.
#[cfg(windows)]
#[allow(unsafe_code)]
fn detached(command: &mut Command) -> Result<()> {
  use std::ffi::c_void;
  use std::io;
  use std::os::windows::io::AsRawHandle;
  use std::os::windows::process::CommandExt;

  const CREATE_NO_WINDOW: u32 = 0x0800_0000;
  const HANDLE_FLAG_INHERIT: u32 = 0x1;
  #[link(name = "kernel32")]
  unsafe extern "system" {
    fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
  }

  for handle in [
    io::stdin().as_raw_handle(),
    io::stdout().as_raw_handle(),
    io::stderr().as_raw_handle(),
  ] {
    // A process without a console has no standard handles: nothing to clear.
    if handle.is_null() {
      continue;
    }
    // SAFETY: the call only changes a flag on a handle value this process holds.
    if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
      return Err(io::Error::last_os_error())
        .context("cannot keep this terminal's handles from the daemon");
    }
  }
  command.creation_flags(CREATE_NO_WINDOW);
  Ok(())
}

/// A process group of its own, so the terminal's hangup and interrupt, which go
/// to its foreground group, do not reach the daemon.
#[cfg(unix)]
fn detached(command: &mut Command) -> Result<()> {
  use std::os::unix::process::CommandExt;
  command.process_group(0);
  Ok(())
}

pub(super) fn start(
  run: &Run,
  store: &Store,
  lead: &str,
  lead_session_id: &str,
  lead_agent: AgentKind,
  poll_interval: Duration,
) -> Result<()> {
  register_lead(run, store, lead, lead_session_id, lead_agent)?;
  store.write(run_record::clear_stop_request)?;
  let mut sizes: HashMap<String, u64> = HashMap::new();
  let mut compacting = false;
  let mut start_recorded = false;
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
    let mut readings = Vec::new();
    for session in store
      .read(|tx| run.sessions(tx))?
      .into_iter()
      .filter(Session::is_live)
    {
      let transcript = session.transcript();
      let size = transcript.size()?;
      let context = transcript.context_size()?;
      let grew = sizes.get(session.name()).copied() != Some(size);
      sizes.insert(session.name().to_owned(), size);
      store.write(|tx| run.record_session_reading(tx, session.id(), context, grew, timestamp))?;
      readings.push((session, context));
    }
    // Recorded once every transcript has been read on the first poll, and git
    // has run, so that `--detach` reports a start only for a daemon that can
    // read what it observes, and before any prompt, which can take longer
    // than `--detach` waits.
    if !start_recorded {
      run.repo().head()?;
      record_run_event(
        store,
        RunEventKind::DaemonStart,
        &format!("pid {}", std::process::id()),
      )?;
      start_recorded = true;
    }
    for (session, context) in readings {
      let transcript = session.transcript();
      let quiet = session.quiet_seconds(timestamp) as f64;
      match session.role() {
        Role::Implementer => {
          observe_implementer(run, store, &session, transcript, quiet)?;
        }
        Role::Commentator => {
          observe_commentator(
            run,
            store,
            &session,
            transcript,
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
    if transcript.output_mentions(abbreviation)? {
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
