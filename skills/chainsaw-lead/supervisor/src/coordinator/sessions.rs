//! Sessions as the supervisor handles them: launching one in the runtime,
//! finding the transcript its agent writes, and the commands that read
//! transcripts back to the lead and the commentator.

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::json;
use sha1::{Digest, Sha1};

use crate::domain::StartSession;
use crate::domain::{RunEventKind, Session, SessionKind, Task};
use crate::infra::transcript_monitor::TranscriptMonitor;
use crate::persistence::store::Store;
use crate::persistence::{prompt, run_event, session};
use crate::run::Run;

pub(super) fn task_session<'r>(
  run: &'r Run,
  store: &Store,
  task: &Task,
) -> Result<Option<Session<'r>>> {
  Ok(match task.session_id() {
    Some(session_id) => store.read(|tx| run.session(tx, session_id))?,
    None => None,
  })
}

/// Where the session's transcript is, or None until its agent has written
/// one. The search can scan every project directory, so a hit is remembered
/// on the session row and never looked for again. Nothing in a run deletes a
/// transcript, so a remembered one that is gone means something outside the
/// run removed it, and that is an error rather than a session reading zero.
pub(super) fn session_transcript(
  run: &Run,
  store: &Store,
  session: &Session,
) -> Result<Option<PathBuf>> {
  if let Some(path) = session.transcript() {
    if !path.is_file() {
      bail!(
        "supervisor: transcript of {} vanished from {}",
        session.name(),
        path.display()
      );
    }
    return Ok(Some(path.to_owned()));
  }
  let found = session
    .agent()
    .transcript(run.dir(), session.external_session_id());
  if let Some(path) = &found {
    store.write(|tx| run.record_session_transcript(tx, session.id(), path))?;
  }
  Ok(found)
}

pub(super) fn session_name(run: &Run, store: &Store, id: Option<i64>) -> Result<String> {
  Ok(match id {
    Some(id) => store
      .read(|tx| run.session(tx, id))?
      .map_or_else(|| "-".to_owned(), |session| session.name().to_owned()),
    None => "-".to_owned(),
  })
}

/// Starts a session on `prompt` and registers it once its agent has begun
/// its transcript. The prompt goes in with the session, so it is recorded as
/// sent once and seen.
pub(super) fn cmd_launch(
  run: &Run,
  store: &Store,
  name: &str,
  kind: SessionKind,
  prompt: &str,
) -> Result<()> {
  let agent = run.settings().launch_agent(kind);
  // The session starts working on its prompt at once, so the tree it was
  // launched on is the one before it starts.
  let launched_head = run.repo().head().ok();
  let launched = run.agent(agent).start(
    run.runtime(),
    StartSession {
      id: name,
      run_dir: run.dir(),
      kind,
      agent,
      args: run.settings().launch_args(kind),
    },
    prompt,
  )?;
  let external_session_id = launched.started.external_id;
  let pane_id = launched.started.pane_id;
  let tab_id = launched.started.tab_id;
  store.write(|tx| {
    session::stop_named(tx, name)?;
    let session = run.register_session(
      tx,
      name,
      kind.role(),
      agent,
      &external_session_id,
      launched_head.as_deref(),
      &launched.transcript,
    )?;
    let sent = prompt::create(tx, session.id(), prompt)?;
    prompt::record_attempt(tx, sent.id())?;
    prompt::record_seen(tx, sent.id())?;
    run_event::create(tx, RunEventKind::Launch, name)?;
    Ok(())
  })?;
  println!(
    "{}",
    json!({"name": name, "pane_id": pane_id, "tab_id": tab_id, "session_id": external_session_id})
  );
  Ok(())
}

pub(super) fn cmd_start_commentator(run: &Run, store: &Store, role_prompt: &Path) -> Result<()> {
  let name = commentator_agent_name(run.dir());
  let role_prompt = absolute_path(role_prompt)?;
  cmd_launch(
    run,
    store,
    &name,
    SessionKind::Commentator,
    &format!(
      "Read and follow this role prompt entirely: {}\nTranscripts directory: {}\nRun directory: {}",
      role_prompt.display(),
      run.transcripts_dir().display(),
      run.dir().display()
    ),
  )
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
  if path.is_absolute() {
    Ok(path.to_owned())
  } else {
    Ok(env::current_dir()?.join(path))
  }
}

fn commentator_agent_name(run_dir: &Path) -> String {
  let digest = Sha1::digest(run_dir.to_string_lossy().as_bytes());
  let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
  format!("commentator-{}", &hex[..8])
}

/// Runs until killed; the commentator drives it under Claude Code's Monitor
/// tool, and each printed line is one wake. A wake is a catch-up on what the
/// implementer did since the commentator's last look, not a review trigger;
/// reviews are triggered by commits.
///
/// Only implementer transcripts count. The commentator's own transcript grows
/// on every wake, so watching it would wake the commentator for the sole
/// reason that it was just woken; the lead's transcript is not its material
/// either.
pub(super) fn cmd_watch_transcripts(run: &Run, store: &Store, interval_ms: u64) -> Result<()> {
  use std::io::Write;

  let mut monitor = TranscriptMonitor::new(&implementer_transcripts(run, store)?);
  loop {
    std::thread::sleep(Duration::from_millis(interval_ms));
    if let Some(line) = monitor.poll(&implementer_transcripts(run, store)?) {
      println!("{line}");
      std::io::stdout().flush()?;
    }
  }
}

/// The transcripts of the live implementers that have one, by session id.
fn implementer_transcripts(run: &Run, store: &Store) -> Result<Vec<(String, PathBuf)>> {
  let mut transcripts = Vec::new();
  for session in store
    .read(|tx| run.sessions(tx))?
    .into_iter()
    .filter(Session::can_take_task)
  {
    if let Some(path) = session_transcript(run, store, &session)? {
      transcripts.push((session.external_session_id().to_owned(), path));
    }
  }
  Ok(transcripts)
}

pub(super) fn cmd_context(run: &Run, store: &Store, name: Option<&str>) -> Result<()> {
  for session in store
    .read(|tx| run.sessions(tx))?
    .into_iter()
    .filter(|session| name.is_none_or(|name| session.name() == name))
  {
    if let Some(transcript) = session_transcript(run, store, &session)? {
      println!(
        "{}\t{}",
        session.name(),
        session.agent().context_size(&transcript)
      );
    } else {
      println!("{}\tUNAVAILABLE (transcript not found)", session.name());
    }
  }
  Ok(())
}
