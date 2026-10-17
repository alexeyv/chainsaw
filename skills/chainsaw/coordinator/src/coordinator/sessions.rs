//! Sessions as the supervisor handles them: launching one in the runtime,
//! and the commands that read transcripts back to the lead and the
//! commentator.

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::domain::StartSession;
use crate::domain::{COMMENTATOR_COMPACT_TOKENS, RunEventKind, Session, SessionKind, Task};
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

pub(super) fn session_name(run: &Run, store: &Store, id: Option<i64>) -> Result<String> {
  Ok(match id {
    Some(id) => store
      .read(|tx| run.session(tx, id))?
      .map_or_else(|| "-".to_owned(), |session| session.name().to_owned()),
    None => "-".to_owned(),
  })
}

/// Where the commentator keeps its notes, in the run's state directory. The
/// role prompt names the same file.
const COMMENTATOR_NOTES_FILE_NAME: &str = "chainsaw-commentator-state.md";

pub(super) fn cmd_launch(
  run: &Run,
  store: &Store,
  name: &str,
  kind: SessionKind,
  prompt: &str,
) -> Result<()> {
  println!("{}", launch(run, store, name, kind, prompt, None)?);
  Ok(())
}

/// Starts a session on `prompt` and registers it once its agent has begun
/// its transcript, a commentator with the `role_prompt` it was told to read.
/// The prompt goes in with the session, so it is recorded as sent once and
/// seen. Returns what the runtime and the agent know the session by.
///
/// A commentator is told at launch to compact itself at
/// `COMMENTATOR_COMPACT_TOKENS`, when its agent can be. That flag goes ahead
/// of the configured ones, so a configured flag of the same name wins.
fn launch(
  run: &Run,
  store: &Store,
  name: &str,
  kind: SessionKind,
  prompt: &str,
  role_prompt: Option<&Path>,
) -> Result<Value> {
  let agent = run.settings().launch_agent(kind);
  let mut args = match kind {
    SessionKind::Commentator => run
      .agent(agent)
      .compact_at_args(COMMENTATOR_COMPACT_TOKENS)
      .unwrap_or_default(),
    SessionKind::Implementer => Vec::new(),
  };
  args.extend_from_slice(run.settings().launch_args(kind));
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
      args: &args,
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
      Some(&pane_id),
      role_prompt,
    )?;
    let sent = prompt::create(tx, session.id(), prompt)?;
    prompt::record_attempt(tx, sent.id())?;
    prompt::record_seen(tx, sent.id())?;
    run_event::create(tx, RunEventKind::Launch, name)?;
    Ok(())
  })?;
  Ok(json!({"name": name, "pane_id": pane_id, "tab_id": tab_id, "session_id": external_session_id}))
}

pub(super) fn cmd_start_commentator(run: &Run, store: &Store, role_prompt: &Path) -> Result<()> {
  let role_prompt = absolute_path(role_prompt)?;
  println!("{}", start_commentator(run, store, &role_prompt, &[])?);
  Ok(())
}

/// Starts the run's commentator on `role_prompt`, told of the commits
/// `waiting` for its review when it is a replacement. Starting it again
/// supersedes the one before.
pub(super) fn start_commentator(
  run: &Run,
  store: &Store,
  role_prompt: &Path,
  waiting: &[Task],
) -> Result<Value> {
  let mut prompt = format!(
    "Read and follow this role prompt entirely: {}\nState directory: {}\nRun directory: {}",
    role_prompt.display(),
    run.state_dir().display(),
    run.dir().display()
  );
  if !waiting.is_empty() {
    prompt.push_str(&format!(
      "\nCommits waiting for your review: {}",
      commit_list(waiting)
    ));
  }
  launch(
    run,
    store,
    &commentator_agent_name(run.dir()),
    SessionKind::Commentator,
    &prompt,
    Some(role_prompt),
  )
}

/// What a commentator whose context was compacted is told: where its role
/// prompt and notes are, to keep its transcript watch running, and which
/// commits wait for its review. One line, as every daemon prompt is.
pub(super) fn commentator_recovery_prompt(
  run: &Run,
  role_prompt: Option<&Path>,
  waiting: &[Task],
) -> String {
  let role_prompt = role_prompt.map_or_else(
    || "your role prompt".to_owned(),
    |path| format!("your role prompt at {}", path.display()),
  );
  let waiting = if waiting.is_empty() {
    "No commit is waiting for your review.".to_owned()
  } else {
    format!("Commits waiting for your review: {}.", commit_list(waiting))
  };
  format!(
    "supervisor: your context was compacted. Reread {role_prompt} and follow it entirely, read your notes at {}, and make sure your `watch-transcripts` monitor is still running. {waiting}",
    run.state_dir().join(COMMENTATOR_NOTES_FILE_NAME).display()
  )
}

/// Each task's commit and the task, as `abc1234 (task 3), def5678 (task 4)`.
fn commit_list(tasks: &[Task]) -> String {
  tasks
    .iter()
    .map(|task| {
      format!(
        "{} (task {})",
        task.commit_sha().unwrap_or_default(),
        task.id()
      )
    })
    .collect::<Vec<_>>()
    .join(", ")
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

/// The transcripts of the live implementers, by session id.
fn implementer_transcripts(run: &Run, store: &Store) -> Result<Vec<(String, PathBuf)>> {
  let mut transcripts = Vec::new();
  for session in store
    .read(|tx| run.sessions(tx))?
    .into_iter()
    .filter(Session::can_take_task)
  {
    let path = session.transcript()?.path().to_owned();
    transcripts.push((session.external_session_id().to_owned(), path));
  }
  Ok(transcripts)
}

pub(super) fn cmd_context(run: &Run, store: &Store, name: Option<&str>) -> Result<()> {
  for session in store
    .read(|tx| run.sessions(tx))?
    .into_iter()
    .filter(|session| name.is_none_or(|name| session.name() == name))
  {
    println!(
      "{}\t{}",
      session.name(),
      session.transcript()?.context_size()
    );
  }
  Ok(())
}
