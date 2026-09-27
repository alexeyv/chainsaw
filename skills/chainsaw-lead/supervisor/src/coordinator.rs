use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Local, TimeZone, Utc};
use fs2::FileExt;
use regex::Regex;
use serde_json::json;
use sha1::{Digest, Sha1};
use strum::IntoEnumIterator;

use crate::cli::{Command, HumanWaitAction, TaskCommand, Verdict};
use crate::domain::{
  AgentKind, ContextSize, FindingVerdict, Role, RunEventKind, Session, SessionKind, Task,
  TaskEvent, TaskState,
};
use crate::infra::agent::{self, Agent, PromptEcho, PromptState};
use crate::infra::git::Repo;
use crate::infra::session_runtime::{SessionRuntime, SessionStatus, StartSession};
use crate::infra::settings::Settings;
use crate::infra::store::{Store, now};
use crate::infra::transcript_monitor::{TranscriptMonitor, transcript_size};
use crate::persistence::{
  calibration, finding, human_wait, observation, prompt, run, run_event, session, task,
};

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
const COMMENTATOR_COMPACT_TOKENS: u64 = 150_000;
const IMPLEMENTER_LIMIT_TOKENS: u64 = 100_000;
const STALE_SECONDS: f64 = 600.0;

/// How many times an agent that echoes its prompts as it takes them is sent
/// one that has not shown up, and so how many prompt timeouts every prompt
/// has to be taken in, however many sends they are spread over.
const PROMPT_ATTEMPTS: i64 = 3;
const VERIFY_LOG_RETRY_SECONDS: u64 = 1;
const COORDINATOR_REMEDY_ONLY: &str = "normally the coordinator records this on its own; use --force --reason only to remedy a coordinator failure";

const CONTRACT: &str = "Verify the tree is clean; stop if dirty. Implement only this task. Run the task's checks as you work; run the project's quality gate once, immediately before committing. Commit without attribution trailers, leave the tree clean, then run exactly `git log -1 --format='[chainsaw %h]'` (the supervisor reads that record), and finish with the commit id, changed-file manifest, a one-paragraph semantic delta, and any gate failures you judged pre-existing (test name and one-line error).";

/// What every command and the daemon act through: the run's store and
/// repository, the runtime its sessions live in, and the run's settings. It
/// carries no behavior of its own; commands are functions over it.
pub struct Coordinator<'a> {
  store: &'a Store,
  repo: Repo<'a>,
  runtime: &'a dyn SessionRuntime,
  settings: &'a Settings,
}

impl<'a> Coordinator<'a> {
  pub fn new(store: &'a Store, runtime: &'a dyn SessionRuntime, settings: &'a Settings) -> Self {
    Self {
      store,
      repo: Repo::new(&store.run_dir),
      runtime,
      settings,
    }
  }
}

pub fn execute(
  store: &Store,
  runtime: &dyn SessionRuntime,
  settings: &Settings,
  command: Command,
) -> Result<()> {
  let coordinator = Coordinator::new(store, runtime, settings);
  let lead_facing = is_lead_facing(&command);
  run(&coordinator, command)?;
  if lead_facing {
    for warning in standing_warnings(&coordinator)? {
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
fn standing_warnings(coordinator: &Coordinator) -> Result<Vec<String>> {
  let mut warnings = Vec::new();
  let at = Utc::now();
  let timestamp = at.timestamp_millis();
  if let Some(lead) = coordinator
    .store
    .read(session::all)?
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
  let (tasks, run) = coordinator
    .store
    .read(|tx| Ok((task::all(tx)?, run::get(tx)?)))?;
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
    let unread = match run.seconds_since_state_read(at) {
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
  let absent = match run.seconds_since_daemon_seen(at) {
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

fn run(coordinator: &Coordinator, command: Command) -> Result<()> {
  match command {
    Command::Daemon {
      lead,
      session_id,
      poll_interval_ms,
    } => daemon(
      coordinator,
      &lead,
      &session_id,
      Duration::from_millis(poll_interval_ms),
    ),
    Command::StartCommentator { role_prompt } => cmd_start_commentator(coordinator, &role_prompt),
    Command::Launch { name } => cmd_launch(coordinator, &name, SessionKind::Implementer),
    Command::Prompt {
      name,
      text,
      wait,
      timeout,
    } => cmd_prompt(coordinator, &name, &text, wait, timeout),
    Command::Task { action } => match action {
      TaskCommand::New {
        files,
        predicted_files,
        predicted_lines,
        retry_of_task_id,
        reason,
      } => cmd_task_new(
        coordinator,
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
      } => cmd_task_record_commit(coordinator, task, &sha, force, reason.as_deref()),
      TaskCommand::RecordCommentary {
        task,
        force,
        reason,
      } => cmd_task_record_commentary(coordinator, task, force, reason.as_deref()),
    },
    Command::Abort { task, reason } => cmd_abort(coordinator, task, &reason),
    Command::Dispatch { task, to, reason } => {
      cmd_dispatch(coordinator, task, &to, reason.as_deref())
    }
    Command::Accept {
      task,
      force,
      reason,
    } => cmd_accept(coordinator, task, force, reason.as_deref()),
    Command::Calibrate { task } => cmd_calibrate(coordinator, task),
    Command::Observe { task, text } => cmd_observe(coordinator, task, &text),
    Command::Finding { task, description } => cmd_finding(coordinator, task, &description),
    Command::Poll {
      after_observation,
      task,
    } => cmd_poll(coordinator, after_observation, task),
    Command::Resolve {
      finding,
      verdict,
      fix_task_id,
      reason,
    } => cmd_resolve(coordinator, finding, &verdict, fix_task_id, &reason),
    Command::Resolutions => cmd_resolutions(coordinator),
    Command::State { task } => cmd_state(coordinator, task),
    Command::TranscriptsDir => {
      println!("{}", coordinator.store.transcripts_dir.display());
      Ok(())
    }
    Command::WatchTranscripts { interval_ms } => cmd_watch_transcripts(coordinator, interval_ms),
    Command::Context { name } => cmd_context(coordinator, name.as_deref()),
    Command::HumanWait { action } => cmd_human_wait(coordinator, action),
    Command::Stop => cmd_stop(coordinator),
  }
}

fn task_session(coordinator: &Coordinator, task: &Task) -> Result<Option<Session>> {
  Ok(match task.session_id() {
    Some(session_id) => coordinator.store.read(|tx| session::get(tx, session_id))?,
    None => None,
  })
}

/// Commit ids the task's session may have made since the task was dispatched;
/// `new_commit_for` decides whether one is really new.
fn task_commits(coordinator: &Coordinator, task: &Task) -> Result<Vec<String>> {
  let Some(session) = task_session(coordinator, task)? else {
    return Ok(Vec::new());
  };
  let Some(transcript) = session_transcript(coordinator, &session)? else {
    return Ok(Vec::new());
  };
  let head = coordinator.repo.head()?;
  Ok(agent::for_session(&session).commit_candidates(
    &transcript,
    task.transcript_offset() as u64,
    &head,
  ))
}

/// Where the session's transcript is, or None until its agent has written
/// one. The search can scan every project directory, so a hit is remembered
/// on the session row and never looked for again. Nothing in a run deletes a
/// transcript, so a remembered one that is gone means something outside the
/// run removed it, and that is an error rather than a session reading zero.
fn session_transcript(coordinator: &Coordinator, session: &Session) -> Result<Option<PathBuf>> {
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
  let found = agent::for_session(session)
    .transcript(&coordinator.store.run_dir, session.external_session_id());
  if let Some(path) = &found {
    coordinator
      .store
      .write(|tx| session::record_transcript(tx, session.id(), path))?;
  }
  Ok(found)
}

fn session_name(coordinator: &Coordinator, id: Option<i64>) -> Result<String> {
  Ok(match id {
    Some(id) => coordinator
      .store
      .read(|tx| session::get(tx, id))?
      .map_or_else(|| "-".to_owned(), |session| session.name().to_owned()),
    None => "-".to_owned(),
  })
}

fn last_task_on(coordinator: &Coordinator, session_id: i64) -> Result<Option<Task>> {
  Ok(
    coordinator
      .store
      .read(|tx| task::tasks_for_session(tx, session_id))?
      .into_iter()
      .rev()
      .find(|task| task.state() != TaskState::Drafted),
  )
}

fn stat_number(text: &str, noun: &str) -> i64 {
  Regex::new(&format!(r"(\d+) {noun}s?"))
    .expect("valid stat regex")
    .captures(text)
    .and_then(|capture| capture[1].parse().ok())
    .unwrap_or_default()
}

fn cmd_launch(coordinator: &Coordinator, name: &str, kind: SessionKind) -> Result<()> {
  let agent = coordinator.settings.launch_agent(kind);
  let started = coordinator.runtime.start(StartSession {
    id: name,
    run_dir: &coordinator.store.run_dir,
    kind,
    agent,
    args: coordinator.settings.launch_args(kind),
  })?;
  let external_session_id = started.external_id;
  let pane_id = started.pane_id;
  let tab_id = started.tab_id;
  let launched_head = coordinator.repo.head().ok();
  coordinator.store.write(|tx| {
    session::stop_named(tx, name)?;
    session::create(
      tx,
      name,
      kind.role(),
      agent,
      &external_session_id,
      launched_head.as_deref(),
    )?;
    run_event::create(tx, RunEventKind::Launch, name)?;
    Ok(())
  })?;
  println!(
    "{}",
    json!({"name": name, "pane_id": pane_id, "tab_id": tab_id, "session_id": external_session_id})
  );
  Ok(())
}

fn cmd_prompt(
  coordinator: &Coordinator,
  name: &str,
  text: &str,
  wait: bool,
  timeout: u64,
) -> Result<()> {
  let lock_path = PathBuf::from(format!("{}.prompt-lock", coordinator.store.path.display()));
  let lock = OpenOptions::new()
    .create(true)
    .write(true)
    .truncate(false)
    .open(lock_path)?;
  lock.lock_exclusive()?;
  // Only the prompt's opening is matched in the transcript.
  let opening: String = text.chars().take(80).collect();
  let prompt_id = coordinator
    .store
    .write(|tx| prompt::create(tx, name, text))?;
  let prompt_timeout_millis = i64::try_from(coordinator.settings.prompt_timeout().as_millis())
    .context("prompt-timeout-seconds is too large")?;
  let session = coordinator
    .store
    .read(|tx| session::latest_named(tx, name))?;
  let transcript = || -> Result<Option<PathBuf>> {
    match &session {
      Some(session) => session_transcript(coordinator, session),
      None => Ok(None),
    }
  };
  let agent = session.as_ref().map(agent::for_session);
  // Every prompt has the same time to be taken in, spread over as many sends
  // as its agent allows: one that echoes a prompt as it takes it can be sent
  // it again; one that echoes it only with its reply may be at work on it.
  let echo = agent.map_or(PromptEcho::OnTake, Agent::prompt_echo);
  let attempts = match echo {
    PromptEcho::OnTake => PROMPT_ATTEMPTS,
    PromptEcho::WithReply => 1,
  };
  let window_millis = prompt_timeout_millis * PROMPT_ATTEMPTS / attempts;

  for attempt in 1..=attempts {
    // Polling the runtime gives it a turn to deliver what a busy session has
    // queued; the state check below then reads what actually arrived. What it
    // reports is the state the send finds the session in.
    let idle_before = status_of(coordinator.runtime, name) == Some(SessionStatus::Idle);
    let path_before = transcript()?;
    let mut offset = transcript_size(path_before.as_deref());
    coordinator
      .store
      .write(|tx| prompt::record_attempt(tx, prompt_id))?;
    let _ = coordinator.runtime.prompt(name, text);
    let deadline = now() + window_millis;
    while now() < deadline {
      let status = status_of(coordinator.runtime, name);
      let path = transcript()?;
      if path != path_before {
        offset = 0;
      }
      if let Some((agent, path)) = agent.zip(path)
        && let state @ (PromptState::Started | PromptState::Queued) =
          agent.prompt_state(&path, offset, &opening)
      {
        coordinator
          .store
          .write(|tx| prompt::record_seen(tx, prompt_id))?;
        if state == PromptState::Queued {
          record_run_event(coordinator, RunEventKind::PromptQueued, name)?;
        }
        return prompt_taken(&lock, coordinator, name, agent, &transcript, wait, timeout);
      }
      // An agent that echoes a prompt only with its reply has taken it once
      // the session the send found idle is busy. A session busy already, on
      // its launch prompt or something else, proves nothing about this one.
      if let Some(agent) = agent
        && echo == PromptEcho::WithReply
        && idle_before
        && status == Some(SessionStatus::Busy)
      {
        record_run_event(
          coordinator,
          RunEventKind::PromptTaken,
          &format!("{name}: session went busy before its transcript showed the prompt"),
        )?;
        return prompt_taken(&lock, coordinator, name, agent, &transcript, wait, timeout);
      }
      thread::sleep(Duration::from_secs(1));
    }
    if attempt < attempts {
      eprintln!("prompt did not show up in the transcript (attempt {attempt}), resending");
    }
  }
  record_run_event(coordinator, RunEventKind::PromptFailed, name)?;
  FileExt::unlock(&lock)?;
  match echo {
    PromptEcho::OnTake => {
      bail!(
        "supervisor: prompt to {name} never showed up in its transcript after {attempts} attempts"
      )
    }
    PromptEcho::WithReply => bail!(
      "supervisor: prompt to {name} never showed up in its transcript and the session never went busy"
    ),
  }
}

/// What the runtime says the session is doing, or None when it has no such
/// session or cannot be reached.
fn status_of(runtime: &dyn SessionRuntime, name: &str) -> Option<SessionStatus> {
  runtime
    .query(name)
    .ok()
    .flatten()
    .map(|session| session.status)
}

/// The prompt is taken: let the next one through and, when asked, wait for
/// the turn to end and print the last thing the agent said.
fn prompt_taken(
  lock: &File,
  coordinator: &Coordinator,
  name: &str,
  agent: &dyn Agent,
  transcript: &dyn Fn() -> Result<Option<PathBuf>>,
  wait: bool,
  timeout: u64,
) -> Result<()> {
  FileExt::unlock(lock)?;
  if wait {
    let _ = coordinator.runtime.wait(name, Duration::from_secs(timeout));
    println!(
      "{}",
      transcript()?
        .and_then(|path| agent.latest_assistant_text(&path))
        .unwrap_or_else(|| "(no assistant text)".to_owned())
    );
  }
  Ok(())
}

fn cmd_start_commentator(coordinator: &Coordinator, role_prompt: &Path) -> Result<()> {
  let name = commentator_agent_name(&coordinator.store.run_dir);
  cmd_launch(coordinator, &name, SessionKind::Commentator)?;
  let role_prompt = absolute_path(role_prompt)?;
  cmd_prompt(
    coordinator,
    &name,
    &format!(
      "Read and follow this role prompt entirely: {}\nTranscripts directory: {}\nRun directory: {}",
      role_prompt.display(),
      coordinator.store.transcripts_dir.display(),
      coordinator.store.run_dir.display()
    ),
    false,
    300,
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

struct NewTaskOptions<'a> {
  predicted_files: Option<i64>,
  predicted_lines: i64,
  retry_of_task_id: Option<i64>,
  files: Option<&'a str>,
  reason: Option<&'a str>,
}

fn cmd_task_new(coordinator: &Coordinator, options: NewTaskOptions<'_>) -> Result<()> {
  let NewTaskOptions {
    mut predicted_files,
    predicted_lines,
    retry_of_task_id,
    files,
    reason,
  } = options;
  let mut text = String::new();
  std::io::stdin().read_to_string(&mut text)?;
  if text.trim().is_empty() {
    bail!("supervisor: task text on stdin is empty");
  }
  let active_retry = match retry_of_task_id {
    Some(retry_of_task_id) => {
      let predecessor = coordinator
        .store
        .read(|tx| task::get(tx, retry_of_task_id))?
        .with_context(|| {
          format!(
            "supervisor: --retry-of {retry_of_task_id} is not aborted, dispatched, or in flight"
          )
        })?;
      match predecessor.state() {
        TaskState::Aborted => None,
        TaskState::Dispatched | TaskState::InFlight => {
          let reason = reason.filter(|reason| !reason.trim().is_empty()).with_context(|| {
            format!(
              "supervisor: --retry-of {retry_of_task_id} requires a non-empty --reason while the predecessor is {}",
              predecessor.state()
            )
          })?;
          Some((retry_of_task_id, reason))
        }
        _ => bail!(
          "supervisor: --retry-of {retry_of_task_id} is not aborted, dispatched, or in flight"
        ),
      }
    }
    None => None,
  };
  let file_list: Vec<_> = files
    .unwrap_or_default()
    .split(',')
    .map(str::trim)
    .filter(|file| !file.is_empty())
    .collect();
  if !file_list.is_empty() {
    let count = i64::try_from(file_list.len()).unwrap_or(i64::MAX);
    if predicted_files.is_some_and(|predicted| predicted != count) {
      bail!(
        "supervisor: --predicted-files {} disagrees with --files ({count} names); give one or the other",
        predicted_files.unwrap_or_default()
      );
    }
    predicted_files = Some(count);
  } else if predicted_files.is_none() {
    bail!("supervisor: task new needs --files a,b,c or --predicted-files N");
  }
  let predicted_files = predicted_files.context("task file prediction was not validated")?;
  let predicted_file_list =
    (!file_list.is_empty()).then(|| file_list.into_iter().map(str::to_owned).collect::<Vec<_>>());
  if let Some((retry_of_task_id, reason)) = active_retry {
    abort_task(coordinator, retry_of_task_id, reason)?;
  }
  let task = coordinator.store.write(|tx| {
    task::create(
      tx,
      &text,
      predicted_files,
      predicted_lines,
      retry_of_task_id,
      predicted_file_list,
    )
  })?;
  println!("{}", task.id());
  Ok(())
}

fn cmd_dispatch(
  coordinator: &Coordinator,
  task_id: i64,
  implementer: &str,
  reason: Option<&str>,
) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: task {task_id} is not in state drafted");
  };
  if task.state() != TaskState::Drafted {
    bail!("supervisor: task {task_id} is not in state drafted");
  }
  let flying = coordinator
    .store
    .read(task::all)?
    .into_iter()
    .find(|task| matches!(task.state(), TaskState::Dispatched | TaskState::InFlight));
  if let Some(flying) = flying {
    bail!(
      "supervisor: an implementer is already in flight ({} is in flight on task {})",
      session_name(coordinator, flying.session_id())?,
      flying.id()
    );
  }
  let Some(session) = coordinator
    .store
    .read(|tx| session::latest_named(tx, implementer))?
  else {
    bail!("supervisor: no session {implementer}; launch it first");
  };
  if !session.can_take_task() {
    if session.is_live() {
      bail!(
        "supervisor: {implementer} is the {}, not an implementer; only implementers take tasks",
        session.role()
      );
    }
    bail!("supervisor: {implementer} is stopped; launch it again first");
  }
  if let Some(prior) = last_task_on(coordinator, session.id())? {
    bail!(
      "supervisor: {implementer} already took task {} ({}); every task gets a fresh implementer",
      prior.id(),
      prior.state()
    );
  }

  let preamble = files_changed_since_launch(coordinator, &session)?;

  let prompt = format!(
    "{}{text}\n\n{CONTRACT}",
    preamble,
    text = task.text().trim_end()
  );
  // The task is measured from where the transcript and the branch stood
  // before the send: an agent may be at work, even past its commit, before
  // its transcript shows the prompt.
  let transcript_offset = transcript_size(session_transcript(coordinator, &session)?.as_deref());
  let base_head = coordinator.repo.head()?;
  // The task is only dispatched once the prompt is taken, so a send that
  // never is leaves it drafted and dispatchable again.
  if let Err(error) = cmd_prompt(coordinator, implementer, &prompt, false, 300) {
    record_run_event(
      coordinator,
      RunEventKind::DispatchFailed,
      &format!("task {task_id} -> {implementer}: {error}"),
    )?;
    return Err(error);
  }
  coordinator.store.write(|tx| {
    task::dispatch(
      tx,
      task_id,
      session.id(),
      transcript_offset as i64,
      &base_head,
      reason,
    )?;
    run_event::create(
      tx,
      RunEventKind::Dispatch,
      &format!("task {task_id} -> {implementer}"),
    )?;
    Ok(())
  })?;
  println!(
    "task {task_id} dispatched to {implementer}; watch `state --task {task_id}` — it prints `{task_id} committed_unverified` when the commit lands"
  );
  Ok(())
}

/// The session may have read the tree before its task arrived; name what moved
/// since it started so it rereads that first. Empty when nothing has.
fn files_changed_since_launch(coordinator: &Coordinator, session: &Session) -> Result<String> {
  let Some(head) = session.launched_head() else {
    return Ok(String::new());
  };
  let files = coordinator.repo.files_changed(head, "HEAD")?;
  if files.is_empty() {
    return Ok(String::new());
  }
  Ok(format!(
    "These files changed since your session started; read them first: {}\n\n",
    files.join(", ")
  ))
}

fn short_sha(sha: &str) -> &str {
  sha.get(..10).unwrap_or(sha)
}

fn new_commit_for(
  coordinator: &Coordinator,
  shas: &[String],
  base_head: Option<&str>,
) -> Result<Option<String>> {
  match base_head {
    Some(base_head) => coordinator.repo.new_commit_among(shas, base_head),
    None => Ok(None),
  }
}

fn forced_remedy_reason<'a>(
  command: &str,
  force: bool,
  reason: Option<&'a str>,
) -> Result<&'a str> {
  match (force, reason) {
    (true, Some(reason)) if !reason.trim().is_empty() => Ok(reason),
    (true, _) => bail!("supervisor: {command} --force requires a non-empty --reason"),
    (false, Some(_)) => {
      bail!("supervisor: --reason only applies with --force; {COORDINATOR_REMEDY_ONLY}")
    }
    (false, None) => bail!("supervisor: {COORDINATOR_REMEDY_ONLY}"),
  }
}

fn cmd_task_record_commit(
  coordinator: &Coordinator,
  task_id: i64,
  sha: &str,
  force: bool,
  reason: Option<&str>,
) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: no task {task_id}");
  };
  let reason = forced_remedy_reason("task record-commit", force, reason)?;
  if !matches!(task.state(), TaskState::Dispatched | TaskState::InFlight) {
    bail!(
      "supervisor: task {task_id} is {}, not awaiting a coordinator-recorded commit",
      task.state()
    );
  }
  let Some(commit_sha) = coordinator.repo.canonical_commit(sha)? else {
    bail!("supervisor: commit {sha} does not exist in the run repository");
  };
  coordinator.store.write(|tx| {
    for other in task::all(tx)? {
      if other.id() != task_id
        && other
          .commit_sha()
          .is_some_and(|recorded| commit_sha.starts_with(recorded))
      {
        bail!(
          "supervisor: commit {sha} is already recorded for task {}",
          other.id()
        );
      }
    }
    let base_head = task.base_head().with_context(|| {
      format!("supervisor: task {task_id} has no base_head to validate the commit against")
    })?;
    if commit_sha
      == coordinator
        .repo
        .canonical_commit(base_head)?
        .unwrap_or_default()
      || !coordinator.repo.is_ancestor(base_head, &commit_sha)?
    {
      bail!(
        "supervisor: commit {sha} does not descend from task {task_id}'s base_head as a new commit"
      );
    }
    task::record_commit(tx, task_id, sha, Some(reason))?;
    run_event::create(
      tx,
      RunEventKind::ForcedCommit,
      &format!("task {task_id} {sha}: {reason}"),
    )?;
    Ok(())
  })?;
  println!("task {task_id} commit recorded by force: {sha}");
  Ok(())
}

fn cmd_task_record_commentary(
  coordinator: &Coordinator,
  task_id: i64,
  force: bool,
  reason: Option<&str>,
) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: no task {task_id}");
  };
  let reason = forced_remedy_reason("task record-commentary", force, reason)?;
  if !matches!(
    task.state(),
    TaskState::CommittedUnverified | TaskState::Accepted
  ) || task.commit_sha().is_none()
  {
    bail!(
      "supervisor: task {task_id} is {}, not ready for commentary delivery",
      task.state()
    );
  }
  coordinator.store.write(|tx| {
    if !task::record_commentary_delivery(tx, task_id)? {
      bail!("supervisor: commentary delivery is already recorded for task {task_id}");
    }
    run_event::create(
      tx,
      RunEventKind::ForcedCommentary,
      &format!("task {task_id}: {reason}"),
    )
  })?;
  println!("task {task_id} commentary delivery recorded by force");
  Ok(())
}

/// Accept a task. Without `--force` this runs the mechanical gate and accepts
/// only if it passes; with it the caller's reason stands in for the gate.
fn cmd_accept(
  coordinator: &Coordinator,
  task_id: i64,
  force: bool,
  reason: Option<&str>,
) -> Result<()> {
  if coordinator
    .store
    .read(|tx| task::get(tx, task_id))?
    .is_none()
  {
    bail!("supervisor: no task {task_id}");
  }
  match (force, reason) {
    (true, Some(reason)) => accept_without_the_gate(coordinator, task_id, reason),
    (true, None) => bail!("supervisor: accept --force requires a non-empty --reason"),
    (false, Some(_)) => {
      bail!("supervisor: --reason only applies with --force; accept without it runs the checks")
    }
    (false, None) => accept_through_the_gate(coordinator, task_id),
  }
}

fn accept_without_the_gate(coordinator: &Coordinator, task_id: i64, reason: &str) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: no task {task_id}");
  };
  if reason.trim().is_empty() {
    bail!("supervisor: accept --force requires a non-empty --reason");
  }
  if task.state() != TaskState::CommittedUnverified || task.commit_sha().is_none() {
    bail!(
      "supervisor: task {task_id} is {}, not a committed unverified task",
      task.state()
    );
  }
  coordinator.store.write(|tx| {
    task::accept(tx, task_id, reason)?;
    run_event::create(
      tx,
      RunEventKind::Accepted,
      &format!("task {task_id}: {reason}"),
    )?;
    Ok(())
  })?;
  println!("task {task_id} accepted without the gate: {reason}");
  Ok(())
}

fn accept_through_the_gate(coordinator: &Coordinator, task_id: i64) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: no task {task_id}");
  };
  let mut sha = match task.commit_sha() {
    Some(sha) => Some(sha.to_owned()),
    None => new_commit_for(
      coordinator,
      &task_commits(coordinator, &task)?,
      task.base_head(),
    )?,
  };
  if sha.is_none() && head_advanced_cleanly(coordinator, task.base_head())? {
    thread::sleep(Duration::from_secs(VERIFY_LOG_RETRY_SECONDS));
    sha = new_commit_for(
      coordinator,
      &task_commits(coordinator, &task)?,
      task.base_head(),
    )?;
  }
  let mut problems = Vec::new();
  if let Some(sha) = &sha {
    match coordinator.repo.commit(sha)? {
      None => problems.push(format!("commit {sha} not in git")),
      Some(commit) => {
        if has_attribution_trailer(&commit.message) {
          problems.push("commit carries an attribution trailer".to_owned());
        }
        if !coordinator.repo.head()?.starts_with(&commit.sha) {
          problems.push("commit is not HEAD".to_owned());
        }
      }
    }
  } else {
    problems.push("no new commit since the task was dispatched".to_owned());
  }
  if !coordinator.repo.is_clean()? {
    problems.push("tree is dirty".to_owned());
  }
  // The implementer runs the quality gate before it commits; that is its
  // contract, and re-deriving it from the transcript only costs wall time.
  if problems.is_empty() {
    let sha = sha
      .as_deref()
      .context("accepted task unexpectedly has no commit")?;
    coordinator.store.write(|tx| {
      task::record_commit(tx, task_id, sha, None)?;
      task::accept(tx, task_id, &format!("checks passed at {sha}"))?;
      Ok(())
    })?;
    println!("task {task_id} accepted: checks passed at {sha}");
    return Ok(());
  }
  println!("task {task_id} NOT accepted:");
  for problem in problems {
    println!(" - {problem}");
  }
  Err(anyhow!(""))
}

fn head_advanced_cleanly(coordinator: &Coordinator, base_head: Option<&str>) -> Result<bool> {
  match base_head {
    Some(base_head) => coordinator.repo.head_advanced_cleanly_from(base_head),
    None => Ok(false),
  }
}

fn has_attribution_trailer(message: &str) -> bool {
  message.lines().any(|line| {
    let lowercase = line.to_ascii_lowercase();
    lowercase.starts_with("co-authored-by:") || lowercase.starts_with("claude-session:")
  })
}

fn failures_in_lineage(coordinator: &Coordinator, task_id: i64) -> Result<i64> {
  let mut failures = 0;
  let mut current = coordinator.store.read(|tx| task::get(tx, task_id))?;
  while let Some(task) = current {
    if task.state() == TaskState::Aborted {
      failures += 1;
    }
    current = match task.retry_of_task_id() {
      Some(retry_of_task_id) => coordinator
        .store
        .read(|tx| task::get(tx, retry_of_task_id))?,
      None => None,
    };
  }
  Ok(failures)
}

fn abort_task(coordinator: &Coordinator, task_id: i64, reason: &str) -> Result<(i64, String)> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: no task {task_id}");
  };
  if reason.trim().is_empty() {
    bail!("supervisor: abort requires a non-empty --reason");
  }
  if task.state().is_terminal() {
    bail!("supervisor: task {task_id} is already {}", task.state());
  }
  let dirty = coordinator.repo.status()?;
  coordinator.store.write(|tx| {
    task::abort(tx, task_id, reason)?;
    run_event::create(
      tx,
      RunEventKind::Aborted,
      &format!("task {task_id}: {reason}"),
    )?;
    Ok(())
  })?;
  if let Some(session_id) = task.session_id()
    && let Some(session) = coordinator.store.read(|tx| session::get(tx, session_id))?
    && session.is_live()
  {
    let detail = format!("task {task_id} -> {}", session.name());
    let outcome = coordinator.runtime.interrupt(session.name()).and_then(|()| {
      daemon_prompt(
        coordinator,
        session.name(),
        &format!(
          "supervisor: task {task_id} is aborted: {reason}. Stop, leave the tree clean, do not commit."
        ),
      )
      .then_some(())
      .with_context(|| format!("abort message did not reach {}", session.name()))
    });
    match outcome {
      Ok(()) => record_run_event(coordinator, RunEventKind::AbortInterrupt, &detail)?,
      Err(error) => record_run_event(
        coordinator,
        RunEventKind::AbortUnreachable,
        &format!("{detail}: {error}"),
      )?,
    }
  }
  let failures = failures_in_lineage(coordinator, task_id)?;
  Ok((failures, dirty))
}

fn cmd_abort(coordinator: &Coordinator, task_id: i64, reason: &str) -> Result<()> {
  let (failures, dirty) = abort_task(coordinator, task_id, reason)?;
  let plural = if failures == 1 { "" } else { "s" };
  println!("task {task_id} aborted ({failures} abort{plural} on this task): {reason}");
  if !dirty.is_empty() {
    println!("WARNING: tree is dirty — the implementer did not leave it clean:\n{dirty}");
  }
  if failures >= 3 {
    println!("three aborts on the same task: escalate to the human");
  } else {
    println!(
      "adjust the task and retry with a fresh implementer: task new --retry-of {task_id} < task.md"
    );
  }
  Ok(())
}

fn cmd_calibrate(coordinator: &Coordinator, task_id: i64) -> Result<()> {
  let Some(task) = coordinator.store.read(|tx| task::get(tx, task_id))? else {
    bail!("supervisor: task {task_id} has no commit yet");
  };
  let Some(commit_sha) = task.commit_sha() else {
    bail!("supervisor: task {task_id} has no commit yet");
  };
  let stat = coordinator.repo.shortstat(commit_sha)?;
  let actual_files = stat_number(&stat, "file");
  let actual_lines = stat_number(&stat, "insertion") + stat_number(&stat, "deletion");
  let dispatched_at = last_event_at(&task, |event| event.state() == TaskState::Dispatched);
  let committed_at = last_event_at(&task, |event| {
    event.state() == TaskState::CommittedUnverified
  });
  let wall = dispatched_at
    .zip(committed_at)
    .map(|(start, end)| (end - start) as f64 / 1000.0);
  let session = task_session(coordinator, &task)?;
  let next_offset = match task.session_id() {
    Some(session_id) => coordinator
      .store
      .read(|tx| task::tasks_for_session(tx, session_id))?
      .into_iter()
      .find(|candidate| candidate.id() > task_id && candidate.transcript_offset() > 0)
      .map(|candidate| candidate.transcript_offset() as u64),
    None => None,
  };
  let peak = match &session {
    Some(session) => match session_transcript(coordinator, session)? {
      Some(transcript) => agent::for_session(session).context_peak(
        &transcript,
        task.transcript_offset() as u64,
        next_offset,
      ),
      None => ContextSize::UNKNOWN,
    },
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
  coordinator.store.write(|tx| {
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

fn cmd_observe(coordinator: &Coordinator, task_id: Option<i64>, text: &str) -> Result<()> {
  let observation = coordinator.store.write(|tx| {
    if let Some(task_id) = task_id {
      require_task(tx, task_id)?;
    }
    observation::create(tx, task_id, text)
  })?;
  println!("{}", observation.id());
  Ok(())
}

fn cmd_finding(coordinator: &Coordinator, task_id: i64, description: &str) -> Result<()> {
  let finding = coordinator.store.write(|tx| {
    require_task(tx, task_id)?;
    finding::register(tx, task_id, description)
  })?;
  println!("{}", finding.id());
  Ok(())
}

fn cmd_poll(coordinator: &Coordinator, after_observation: i64, task_id: Option<i64>) -> Result<()> {
  if after_observation < 0 {
    bail!("supervisor: --after-observation must be nonnegative");
  }
  let (observations, findings) = coordinator.store.read(|tx| {
    if let Some(task_id) = task_id {
      require_task(tx, task_id)?;
    }
    Ok((
      observation::after(tx, after_observation, task_id)?,
      finding::unresolved(tx, task_id)?,
    ))
  })?;
  let observation_cursor = observations
    .last()
    .map_or(after_observation, |observation| observation.id());
  let observations = observations
    .into_iter()
    .map(|observation| {
      json!({
        "id": observation.id(),
        "task_id": observation.task_id(),
        "text": observation.text(),
        "created_at": observation.created_at().to_rfc3339(),
      })
    })
    .collect::<Vec<_>>();
  let findings = findings
    .into_iter()
    .map(|finding| {
      json!({
        "id": finding.id(),
        "task_id": finding.task_id(),
        "description": finding.description(),
        "created_at": finding.created_at().to_rfc3339(),
      })
    })
    .collect::<Vec<_>>();
  println!(
    "{}",
    json!({
      "observation_cursor": observation_cursor,
      "observations": observations,
      "findings": findings,
    })
  );
  Ok(())
}

fn cmd_resolve(
  coordinator: &Coordinator,
  finding_id: i64,
  verdict: &Verdict,
  fix_task_id: Option<i64>,
  reason: &str,
) -> Result<()> {
  let verdict = match verdict {
    Verdict::Task => FindingVerdict::Task,
    Verdict::Dropped => FindingVerdict::Dropped,
  };
  coordinator.store.write(|tx| {
    let finding = finding::get(tx, finding_id)?
      .with_context(|| format!("supervisor: no finding {finding_id}"))?;
    if let Some(fix_task_id) = fix_task_id {
      require_task(tx, fix_task_id)?;
    }
    finding::resolve(tx, &finding, verdict, reason, fix_task_id)
      .map_err(|error| anyhow!("supervisor: {error}"))?;
    Ok(())
  })?;
  println!("finding {finding_id} resolved");
  Ok(())
}

fn cmd_resolutions(coordinator: &Coordinator) -> Result<()> {
  let resolutions = coordinator
    .store
    .read(finding::resolved)?
    .into_iter()
    .map(|finding| {
      json!({
        "finding_id": finding.id(),
        "task_id": finding.task_id(),
        "description": finding.description(),
        "verdict": finding.verdict().map(FindingVerdict::as_str),
        "reason": finding.verdict_reason(),
        "fix_task_id": finding.fix_task_id(),
        "resolved_at": finding.resolved_at().map(|time| time.to_rfc3339()),
      })
    })
    .collect::<Vec<_>>();
  println!("{}", json!({"resolutions": resolutions}));
  Ok(())
}

fn require_task(transaction: &rusqlite::Transaction<'_>, task_id: i64) -> Result<Task> {
  task::get(transaction, task_id)?.with_context(|| format!("supervisor: no task {task_id}"))
}

fn cmd_state(coordinator: &Coordinator, only_task: Option<i64>) -> Result<()> {
  coordinator.store.write(run::record_state_read)?;
  if let Some(task_id) = only_task {
    let task = coordinator
      .store
      .read(|tx| task::get(tx, task_id))?
      .with_context(|| format!("supervisor: no task {task_id}"))?;
    println!("{task_id} {}", task.state());
    return Ok(());
  }
  println!("tasks");
  let tasks = coordinator.store.read(task::all)?;
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
      session_name(coordinator, task.session_id())?,
      task.commit_sha().map(short_sha).unwrap_or("-")
    );
  }
  println!("sessions");
  for session in coordinator.store.read(session::all)? {
    let mut flags = String::new();
    let implementer = session.role() == Role::Implementer;
    if implementer && session.context().exceeds(IMPLEMENTER_LIMIT_TOKENS) {
      flags.push_str(" OVER-LIMIT");
    }
    let quiet = session.quiet_seconds(Utc::now());
    if session_transcript(coordinator, &session)?.is_some() {
      println!(
        "  {:<16} {:<12} context {:>7} (max {}) quiet {quiet}s{flags}",
        session.name(),
        session.role(),
        session.context(),
        session.context_max()
      );
    } else {
      let danger = if session.role() == Role::Lead {
        "; lead stop threshold disabled"
      } else {
        ""
      };
      println!(
        "  {:<16} {:<12} context UNAVAILABLE (transcript not found{danger}) quiet {quiet}s{flags}",
        session.name(),
        session.role()
      );
    }
  }
  print_time_summary(coordinator)?;
  if coordinator.store.read(human_wait::is_open)? {
    println!("  (a human wait is open)");
  }
  let events = coordinator
    .store
    .read(|tx| run_event::recent(tx, STATE_EVENT_KINDS, 5))?;
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

/// Journals one supervisor action that belongs to no other write.
fn record_run_event(coordinator: &Coordinator, kind: RunEventKind, detail: &str) -> Result<()> {
  coordinator
    .store
    .write(|tx| run_event::create(tx, kind, detail))?;
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

fn clock_time(millis: i64) -> String {
  Local.timestamp_millis_opt(millis).single().map_or_else(
    || "-".to_owned(),
    |time| time.format("%H:%M:%S").to_string(),
  )
}

fn print_time_summary(coordinator: &Coordinator) -> Result<()> {
  let tasks = coordinator.store.read(task::all)?;
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
      busy += end.unwrap_or_else(now) - start;
    }
  }
  let mut human = 0;
  for (start, end) in coordinator.store.read(human_wait::intervals)? {
    human += end.unwrap_or_else(now) - start;
  }
  if let Some(first) = first {
    let wall = now() - first;
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

/// Runs until killed; the commentator drives it under Claude Code's Monitor
/// tool, and each printed line is one wake. A wake is a catch-up on what the
/// implementer did since the commentator's last look, not a review trigger;
/// reviews are triggered by commits.
///
/// Only implementer transcripts count. The commentator's own transcript grows
/// on every wake, so watching it would wake the commentator for the sole
/// reason that it was just woken; the lead's transcript is not its material
/// either.
fn cmd_watch_transcripts(coordinator: &Coordinator, interval_ms: u64) -> Result<()> {
  use std::io::Write;

  let mut monitor = TranscriptMonitor::new(&implementer_transcripts(coordinator)?);
  loop {
    std::thread::sleep(Duration::from_millis(interval_ms));
    if let Some(line) = monitor.poll(&implementer_transcripts(coordinator)?) {
      println!("{line}");
      std::io::stdout().flush()?;
    }
  }
}

/// The transcripts of the live implementers that have one, by session id.
fn implementer_transcripts(coordinator: &Coordinator) -> Result<Vec<(String, PathBuf)>> {
  let mut transcripts = Vec::new();
  for session in coordinator
    .store
    .read(session::all)?
    .into_iter()
    .filter(Session::can_take_task)
  {
    if let Some(path) = session_transcript(coordinator, &session)? {
      transcripts.push((session.external_session_id().to_owned(), path));
    }
  }
  Ok(transcripts)
}

fn cmd_context(coordinator: &Coordinator, name: Option<&str>) -> Result<()> {
  for session in coordinator
    .store
    .read(session::all)?
    .into_iter()
    .filter(|session| name.is_none_or(|name| session.name() == name))
  {
    if let Some(transcript) = session_transcript(coordinator, &session)? {
      println!(
        "{}\t{}",
        session.name(),
        agent::for_session(&session).context_size(&transcript)
      );
    } else {
      println!("{}\tUNAVAILABLE (transcript not found)", session.name());
    }
  }
  Ok(())
}

fn cmd_human_wait(coordinator: &Coordinator, action: HumanWaitAction) -> Result<()> {
  match action {
    HumanWaitAction::Start => coordinator.store.write(human_wait::start)?,
    HumanWaitAction::End => coordinator.store.write(human_wait::end)?,
  };
  Ok(())
}

fn cmd_stop(coordinator: &Coordinator) -> Result<()> {
  coordinator.store.write(|tx| {
    run::request_stop(tx)?;
    run_event::create(tx, RunEventKind::Stop, "run ended by the lead")?;
    Ok(())
  })?;
  println!("supervisor: stopped; the daemon will exit on its next poll");
  Ok(())
}

fn daemon_prompt(coordinator: &Coordinator, name: &str, text: &str) -> bool {
  match cmd_prompt(coordinator, name, text, false, 300) {
    Ok(()) => true,
    Err(error) => {
      let _ = record_run_event(
        coordinator,
        RunEventKind::PromptUnreachable,
        &format!("{name}: {error}"),
      );
      false
    }
  }
}

fn daemon(
  coordinator: &Coordinator,
  lead: &str,
  lead_session_id: &str,
  poll_interval: Duration,
) -> Result<()> {
  register_lead(coordinator, lead, lead_session_id)?;
  coordinator.store.write(|tx| {
    run::clear_stop_request(tx)?;
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
    let stopping = coordinator.store.write(|tx| {
      let stopping = run::get(tx)?.is_stopping();
      if !stopping {
        run::record_daemon_seen(tx)?;
      }
      Ok(stopping)
    })?;
    if stopping {
      break;
    }
    let timestamp = Utc::now();
    for session in coordinator
      .store
      .read(session::all)?
      .into_iter()
      .filter(Session::is_live)
    {
      let name = session.name();
      let Some(transcript) = session_transcript(coordinator, &session)? else {
        if missing_transcripts.insert(name.to_owned()) {
          let danger = if session.role() == Role::Lead {
            "; the lead context stop threshold cannot fire"
          } else {
            ""
          };
          let detail = format!("{name} ({}): transcript not found{danger}", session.role());
          eprintln!("WARNING: {detail}");
          record_run_event(coordinator, RunEventKind::TranscriptMissing, &detail)?;
        }
        continue;
      };
      if missing_transcripts.remove(name) {
        eprintln!(
          "supervisor: transcript found for {name}: {}",
          transcript.display()
        );
        record_run_event(
          coordinator,
          RunEventKind::TranscriptFound,
          &format!("{name}: {}", transcript.display()),
        )?;
      }
      let size = transcript_size(Some(&transcript));
      let context = agent::for_session(&session).context_size(&transcript);
      let grew = sizes.get(name).copied() != Some(size);
      sizes.insert(name.to_owned(), size);
      coordinator
        .store
        .write(|tx| session::record_reading(tx, session.id(), context, grew, timestamp))?;
      let quiet = session.quiet_seconds(timestamp) as f64;

      match session.role() {
        Role::Implementer => {
          observe_implementer(coordinator, &session, &transcript, quiet)?;
        }
        Role::Commentator => {
          observe_commentator(
            coordinator,
            &session,
            Reading {
              transcript: &transcript,
              context,
              quiet,
            },
            &mut compacting,
          )?;
        }
        Role::Lead => observe_lead(coordinator, &session, context)?,
      }
    }
    thread::sleep(poll_interval);
  }
  record_run_event(
    coordinator,
    RunEventKind::DaemonExit,
    &format!("pid {}", std::process::id()),
  )
}

/// The lead is started by the human in Claude Code, so the daemon registers
/// it from what the lead says about itself. The same session id keeps its row
/// across daemon restarts; a different one is a new incarnation and stops the
/// old row.
fn register_lead(coordinator: &Coordinator, lead: &str, lead_session_id: &str) -> Result<()> {
  coordinator.store.write(|tx| {
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
fn kick_if_stalled(coordinator: &Coordinator, session: &Session, quiet: f64) -> Result<()> {
  if quiet > STALE_SECONDS
    && session.can_be_kicked()
    && status_of(coordinator.runtime, session.name()) == Some(SessionStatus::Idle)
    && daemon_prompt(coordinator, session.name(), "continue")
  {
    coordinator.store.write(|tx| {
      session::record_kick(tx, session.id())?;
      run_event::create(tx, RunEventKind::Kick, session.name())?;
      Ok(())
    })?;
  }
  Ok(())
}

fn observe_implementer(
  coordinator: &Coordinator,
  session: &Session,
  transcript: &Path,
  quiet: f64,
) -> Result<()> {
  let agent = agent::for_session(session);
  let task = coordinator
    .store
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
    coordinator
      .store
      .write(|tx| task::take_flight(tx, task.id(), context))?;
    return Ok(());
  }
  let head = coordinator.repo.head()?;
  let shas = agent.commit_candidates(transcript, task.transcript_offset() as u64, &head);
  if let Some(sha) = new_commit_for(coordinator, &shas, task.base_head())? {
    coordinator.store.write(|tx| {
      task::record_commit(tx, task.id(), &sha, None)?;
      run_event::create(
        tx,
        RunEventKind::Committed,
        &format!("task {} {sha}", task.id()),
      )?;
      Ok(())
    })?;
  } else {
    kick_if_stalled(coordinator, session, quiet)?;
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
  coordinator: &Coordinator,
  session: &Session,
  reading: Reading<'_>,
  compacting: &mut bool,
) -> Result<()> {
  let Reading {
    transcript,
    context,
    quiet,
  } = reading;
  let pending = coordinator
    .store
    .read(task::all)?
    .into_iter()
    .filter(Task::awaits_commentary)
    .collect::<Vec<_>>();
  let agent = agent::for_session(session);
  for task in pending {
    let sha = task.commit_sha().unwrap_or_default();
    let abbreviation = sha.get(..7).unwrap_or(sha);
    if agent.output_mentions(transcript, abbreviation) {
      coordinator.store.write(|tx| {
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
        coordinator,
        session.name(),
        &format!(
          "supervisor: commit {sha} landed for task {}; review it from git",
          task.id()
        ),
      )
    {
      coordinator.store.write(|tx| {
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
    if daemon_prompt(coordinator, session.name(), agent.compact_prompt()) {
      *compacting = true;
      record_run_event(
        coordinator,
        RunEventKind::Compact,
        &format!("{} at {context}", session.name()),
      )?;
    }
  } else if context.is_under(COMMENTATOR_COMPACT_TOKENS) {
    *compacting = false;
  }
  kick_if_stalled(coordinator, session, quiet)
}

/// Records the lead crossing its stop threshold once per lead session. Nothing
/// is pushed at the lead: an unsolicited prompt mid-thought is a context switch
/// it did not choose. The warning printed after every lead-facing command
/// carries the same fact at the moment the lead is already reading output.
fn observe_lead(coordinator: &Coordinator, session: &Session, context: ContextSize) -> Result<()> {
  if context.exceeds(LEAD_STOP_TOKENS) && session.can_latch_over_limit() {
    coordinator.store.write(|tx| {
      session::record_over_limit(tx, session.id())?;
      run_event::create(tx, RunEventKind::StopLead, &format!("context {context}"))?;
      Ok(())
    })?;
  }
  Ok(())
}
