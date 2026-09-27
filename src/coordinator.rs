use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Local, TimeZone, Utc};
use regex::Regex;
use serde_json::json;
use strum::IntoEnumIterator;

use crate::cli::{Command, HumanWaitAction, TaskCommand, Verdict};
use crate::domain::{
  ContextSize, FindingVerdict, Role, RunEventKind, Session, SessionKind, Task, TaskEvent, TaskState,
};
use crate::infra::agent;
use crate::infra::git::Repo;
use crate::infra::session_runtime::SessionRuntime;
use crate::infra::settings::Settings;
use crate::infra::store::{Store, now};
use crate::persistence::{
  calibration, finding, human_wait, observation, run, run_event, session, task,
};

mod daemon;
mod prompt;
mod sessions;
mod tasks;

use prompt::{cmd_prompt, daemon_prompt, status_of};
use sessions::{
  cmd_context, cmd_launch, cmd_start_commentator, cmd_watch_transcripts, session_name,
  session_transcript, task_session,
};
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
const IMPLEMENTER_LIMIT_TOKENS: u64 = 100_000;
const VERIFY_LOG_RETRY_SECONDS: u64 = 1;
const COORDINATOR_REMEDY_ONLY: &str = "normally the coordinator records this on its own; use --force --reason only to remedy a coordinator failure";

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
    } => daemon::run(
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

fn stat_number(text: &str, noun: &str) -> i64 {
  Regex::new(&format!(r"(\d+) {noun}s?"))
    .expect("valid stat regex")
    .captures(text)
    .and_then(|capture| capture[1].parse().ok())
    .unwrap_or_default()
}

fn short_sha(sha: &str) -> &str {
  sha.get(..10).unwrap_or(sha)
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
