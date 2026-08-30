use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Local, TimeZone, Utc};
use fs2::FileExt;
use regex::Regex;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use strum::IntoEnumIterator;

use crate::cli::{Command, HumanWaitAction, TaskCommand, Verdict};
use crate::domain::{FindingVerdict, Role, Session, Task, TaskEvent, TaskState};
use crate::logs::{
  PromptLanding, commits_in_log, context_before, context_peak, context_size, file_size,
  format_growth, latest_assistant_text, prompt_landed, transcript_growth, transcript_sizes,
};
use crate::persistence::{calibration, commentary_delivery, finding, observation, session, task};
use crate::session_runtime::{SessionKind, SessionRuntime, StartSession};
use crate::settings::Settings;
use crate::store::{Store, now};

const LEAD_STOP_TOKENS: i64 = 250_000;
const LEAD_WARN_TOKENS: i64 = 200_000;
/// A commit the lead has not judged after this long is being missed.
const COMMIT_UNATTENDED_SECONDS: i64 = 300;
/// Task monitors read state every few seconds; this much silence while a task
/// is out means nothing is watching.
const STATE_UNREAD_SECONDS: i64 = 120;
/// The daemon stamps `daemon-seen` on every poll; silence this long means no
/// daemon is running, whether it never started, was stopped, or died.
const DAEMON_SILENT_SECONDS: i64 = 30;
const COMMENTATOR_COMPACT_TOKENS: i64 = 150_000;
const IMPLEMENTER_LIMIT_TOKENS: i64 = 100_000;
const STALE_SECONDS: f64 = 600.0;
const PROMPT_ATTEMPTS: i64 = 3;
const VERIFY_LOG_RETRY_SECONDS: u64 = 1;
const COORDINATOR_REMEDY_ONLY: &str = "normally the coordinator records this on its own; use --force --reason only to remedy a coordinator failure";

const CONTRACT: &str = "Verify the tree is clean; stop if dirty. Implement only this task. Run the task's checks as you work; run the project's quality gate once, immediately before committing. Commit without attribution trailers, leave the tree clean, then run exactly `git log -1 --format='[chainsaw %h]'` (the supervisor reads that record), and finish with the commit id, changed-file manifest, a one-paragraph semantic delta, and any gate failures you judged pre-existing (test name and one-line error).";

pub fn execute(store: &Store, runtime: &dyn SessionRuntime, command: Command) -> Result<()> {
  let lead_facing = is_lead_facing(&command);
  run(store, runtime, command)?;
  if lead_facing {
    for warning in standing_warnings(store)? {
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
      | Command::LogsDir
      | Command::Context { .. }
      | Command::Config { .. }
      | Command::Stop
  )
}

/// Facts the lead must act on, printed after every lead-facing command so
/// they do not depend on the lead remembering the skill. Each one is measured
/// from the store, never inferred from what the lead said.
fn standing_warnings(store: &Store) -> Result<Vec<String>> {
  let mut warnings = Vec::new();
  let timestamp = now();
  if let Some(lead) = session_snapshots(store)?
    .into_iter()
    .find(|session| session.role() == Role::Lead && session.is_live())
  {
    let context = lead.context();
    if context > LEAD_STOP_TOKENS {
      warnings.push(format!(
        "lead context {context} is past {LEAD_STOP_TOKENS}: stop the run per the skill's Stopping section"
      ));
    } else if context > LEAD_WARN_TOKENS {
      warnings.push(format!("lead context {context} of {LEAD_STOP_TOKENS}"));
    }
  }
  let transaction = store.db.unchecked_transaction()?;
  let tasks = task::all(&transaction)?;
  transaction.commit()?;
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
    let last_read = store
      .cfg("last-state-read")?
      .and_then(|value| value.parse::<i64>().ok());
    let unread = match last_read {
      Some(read) if (timestamp - read) / 1000 <= STATE_UNREAD_SECONDS => None,
      Some(read) => Some(format!(
        "no state read for {}",
        duration_text((timestamp - read) / 1000)
      )),
      None => Some("state has never been read".to_owned()),
    };
    if let Some(unread) = unread {
      warnings.push(format!(
        "{unread} while task {task_id} is out: is a monitor armed on `state --task {task_id}`?"
      ));
    }
  }
  let seen = store
    .cfg("daemon-seen")?
    .and_then(|value| value.parse::<i64>().ok());
  let absent = match seen {
    Some(seen) if (timestamp - seen) / 1000 <= DAEMON_SILENT_SECONDS => None,
    Some(seen) => Some(format!(
      "no daemon has polled for {}",
      duration_text((timestamp - seen) / 1000)
    )),
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

fn run(store: &Store, runtime: &dyn SessionRuntime, command: Command) -> Result<()> {
  match command {
    Command::Daemon {
      lead,
      session_id,
      poll_interval_ms,
    } => daemon(
      store,
      runtime,
      &lead,
      &session_id,
      Duration::from_millis(poll_interval_ms),
    ),
    Command::StartCommentator { role_prompt } => {
      cmd_start_commentator(store, runtime, &role_prompt)
    }
    Command::Launch {
      name,
      fresh,
      reason,
    } => cmd_launch(
      store,
      runtime,
      &name,
      LaunchOptions {
        role: Role::Implementer,
        kind: SessionKind::Implementer,
        fresh,
        reason: reason.as_deref(),
      },
    ),
    Command::Prompt {
      name,
      text,
      wait,
      timeout,
    } => cmd_prompt(store, runtime, &name, &text, wait, timeout),
    Command::Task { action } => match action {
      TaskCommand::New {
        files,
        predicted_files,
        predicted_lines,
        retry_of_task_id,
        reason,
      } => cmd_task_new(
        store,
        runtime,
        predicted_files,
        predicted_lines,
        retry_of_task_id,
        files.as_deref(),
        reason.as_deref(),
      ),
      TaskCommand::RecordCommit {
        task,
        sha,
        force,
        reason,
      } => cmd_task_record_commit(store, task, &sha, force, reason.as_deref()),
      TaskCommand::RecordCommentary {
        task,
        force,
        reason,
      } => cmd_task_record_commentary(store, task, force, reason.as_deref()),
    },
    Command::Abort { task, reason } => cmd_abort(store, runtime, task, &reason),
    Command::Dispatch {
      task,
      to,
      reuse,
      reason,
    } => cmd_dispatch(store, runtime, task, &to, reuse, reason.as_deref()),
    Command::Accept {
      task,
      force,
      reason,
    } => cmd_accept(store, task, force, reason.as_deref()),
    Command::Calibrate { task } => cmd_calibrate(store, task),
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
    Command::Config { key, value } => {
      if let Some(value) = value {
        store.set_cfg(&key, &value)
      } else {
        println!("{}", store.cfg_or(&key, "")?);
        Ok(())
      }
    }
    Command::State { task } => cmd_state(store, task),
    Command::LogsDir => {
      println!("{}", store.logs_dir.display());
      Ok(())
    }
    Command::WatchTranscripts { interval_ms } => cmd_watch_transcripts(store, interval_ms),
    Command::Context { name } => cmd_context(store, name.as_deref()),
    Command::HumanWait { action } => cmd_human_wait(store, action),
    Command::Stop => cmd_stop(store),
  }
}

struct LaunchOptions<'a> {
  role: Role,
  kind: SessionKind,
  fresh: bool,
  reason: Option<&'a str>,
}

/// The session's transcript: beside the database when Claude Code agrees about
/// the project directory, otherwise wherever it was found under the projects
/// directory. None until the transcript exists.
fn session_log(store: &Store, session: &Session) -> Option<PathBuf> {
  let expected = store
    .logs_dir
    .join(format!("{}.jsonl", session.external_session_id()));
  if expected.is_file() {
    Some(expected)
  } else {
    find_session_log(store, session.external_session_id())
  }
}

fn find_session_log(store: &Store, external_session_id: &str) -> Option<PathBuf> {
  let projects_dir = store.logs_dir.parent()?;
  let filename = format!("{external_session_id}.jsonl");
  fs::read_dir(projects_dir)
    .ok()?
    .filter_map(Result::ok)
    .map(|entry| entry.path().join(&filename))
    .find(|path| path.is_file())
}

fn session_log_named(store: &Store, name: &str) -> Result<Option<PathBuf>> {
  Ok(latest_session_named(store, name)?.and_then(|session| session_log(store, &session)))
}

fn session_snapshot(store: &Store, id: i64) -> Result<Option<Session>> {
  let transaction = store.db.unchecked_transaction()?;
  let found = session::get(&transaction, id)?;
  transaction.commit()?;
  Ok(found)
}

fn latest_session_named(store: &Store, name: &str) -> Result<Option<Session>> {
  let transaction = store.db.unchecked_transaction()?;
  let found = session::latest_named(&transaction, name)?;
  transaction.commit()?;
  Ok(found)
}

fn session_snapshots(store: &Store) -> Result<Vec<Session>> {
  let transaction = store.db.unchecked_transaction()?;
  let sessions = session::all(&transaction)?;
  transaction.commit()?;
  Ok(sessions)
}

fn session_name(store: &Store, id: Option<i64>) -> Result<String> {
  Ok(match id {
    Some(id) => session_snapshot(store, id)?
      .map_or_else(|| "-".to_owned(), |session| session.name().to_owned()),
    None => "-".to_owned(),
  })
}

fn git(store: &Store, args: &[&str]) -> Result<Output> {
  ProcessCommand::new("git")
    .arg("-C")
    .arg(&store.run_dir)
    .args(args)
    .output()
    .context("failed to run git")
}

fn git_stdout(store: &Store, args: &[&str]) -> Result<String> {
  Ok(
    String::from_utf8_lossy(&git(store, args)?.stdout)
      .trim()
      .to_owned(),
  )
}

fn last_task_on(store: &Store, session_id: i64) -> Result<Option<Task>> {
  Ok(
    task_snapshots_for_session(store, session_id)?
      .into_iter()
      .rev()
      .find(|task| task.state() != TaskState::Drafted),
  )
}

fn last_seen_commit(store: &Store, session_id: i64) -> Result<Option<String>> {
  Ok(
    last_task_on(store, session_id)?
      .and_then(|task| task.commit_sha().or(task.base_head()).map(str::to_owned)),
  )
}

fn staleness(store: &Store, since: Option<&str>) -> Result<(i64, i64, i64)> {
  let Some(since) = since else {
    return Ok((0, 0, 0));
  };
  let range = format!("{since}..HEAD");
  let commits = git_stdout(store, &["rev-list", "--count", &range])?
    .parse()
    .unwrap_or_default();
  let stat = git_stdout(store, &["diff", "--shortstat", since, "HEAD"])?;
  Ok((
    commits,
    stat_number(&stat, "file"),
    stat_number(&stat, "insertion") + stat_number(&stat, "deletion"),
  ))
}

fn stat_number(text: &str, noun: &str) -> i64 {
  Regex::new(&format!(r"(\d+) {noun}s?"))
    .expect("valid stat regex")
    .captures(text)
    .and_then(|capture| capture[1].parse().ok())
    .unwrap_or_default()
}

fn authored_files(store: &Store, session_id: i64) -> Result<Vec<String>> {
  let mut files = Vec::new();
  let mut seen = HashSet::new();
  for task in task_snapshots_for_session(store, session_id)?
    .into_iter()
    .filter(|task| task.commit_sha().is_some())
  {
    let sha = task.commit_sha().unwrap_or_default();
    for file in git_stdout(store, &["show", "--name-only", "--format=", sha])?.lines() {
      if seen.insert(file.to_owned()) {
        files.push(file.to_owned());
      }
    }
  }
  Ok(files)
}

fn reuse_verdict(store: &Store, session: &Session) -> Result<Option<String>> {
  if !session.is_live() {
    return Ok(Some("session is stopped".to_owned()));
  }
  let tasks = task_snapshots_for_session(store, session.id())?;
  if tasks
    .iter()
    .any(|task| matches!(task.state(), TaskState::Dispatched | TaskState::InFlight))
  {
    return Ok(Some("session is in flight".to_owned()));
  }
  let last = tasks
    .into_iter()
    .rev()
    .find(|task| task.state() != TaskState::Drafted);
  if let Some(task) = &last
    && task.state() == TaskState::Aborted
  {
    return Ok(Some(format!(
      "its last task ({}) aborted; a retry gets a fresh head",
      task.id()
    )));
  }
  let settings = Settings::load(&store.run_dir)?;
  let context_limit = settings.reuse_max_context();
  if session.context() > context_limit {
    return Ok(Some(format!(
      "context {} is over reuse-max-context {context_limit}",
      session.context()
    )));
  }
  let since = last.and_then(|task| task.commit_sha().or(task.base_head()).map(str::to_owned));
  let (commits, files, lines) = staleness(store, since.as_deref())?;
  let stale_limit = settings.reuse_max_stale_lines();
  if lines > stale_limit {
    return Ok(Some(format!(
      "tree moved {lines} lines in {files} files over {commits} commits since its last turn, over reuse-max-stale-lines {stale_limit}: its memory of the tree is wrong, not merely old"
    )));
  }
  Ok(None)
}

#[derive(Clone)]
struct IdleSession {
  session: Session,
  stale: (i64, i64, i64),
  files: Vec<String>,
}

fn idle_pool(store: &Store) -> Result<Vec<IdleSession>> {
  let mut pool = Vec::new();
  for session in session_snapshots(store)?
    .into_iter()
    .filter(|session| session.role() == Role::Implementer)
  {
    if reuse_verdict(store, &session)?.is_some() {
      continue;
    }
    let since = last_seen_commit(store, session.id())?;
    pool.push(IdleSession {
      stale: staleness(store, since.as_deref())?,
      files: authored_files(store, session.id())?,
      session,
    });
  }
  Ok(pool)
}

fn describe_idle(store: &Store, idle: &IdleSession) -> Result<String> {
  let session = &idle.session;
  if last_task_on(store, session.id())?.is_none() {
    return Ok(format!(
      "{} is idle at context {} and has never taken a task — dispatch <task-id> --to {}",
      session.name(),
      session.context(),
      session.name()
    ));
  }
  let (commits, files, lines) = idle.stale;
  let authored = if idle.files.is_empty() {
    "nothing yet".to_owned()
  } else {
    idle.files.join(", ")
  };
  Ok(format!(
    "{} is idle at context {}, tree moved {lines} lines/{files} files/{commits} commits since its last turn, authored {authored} — dispatch <task-id> --to {} --reuse",
    session.name(),
    session.context(),
    session.name()
  ))
}

fn cmd_launch(
  store: &Store,
  runtime: &dyn SessionRuntime,
  name: &str,
  options: LaunchOptions<'_>,
) -> Result<()> {
  if options.role == Role::Implementer {
    let pool: Vec<_> = idle_pool(store)?
      .into_iter()
      .filter(|idle| idle.session.name() != name)
      .collect();
    if !pool.is_empty() && !options.fresh {
      eprintln!("supervisor: launch {name} refused — an idle implementer can take the next task:");
      for idle in &pool {
        eprintln!("  {}", describe_idle(store, idle)?);
      }
      bail!(
        "dispatch to one of those, or launch {name} --fresh --reason \"...\" to record why a fresh head is needed"
      );
    }
    if options.fresh {
      let Some(reason) = options.reason.filter(|reason| !reason.trim().is_empty()) else {
        bail!("supervisor: --fresh requires a non-empty --reason");
      };
      let idle = if pool.is_empty() {
        "none".to_owned()
      } else {
        pool
          .iter()
          .map(|item| item.session.name())
          .collect::<Vec<_>>()
          .join(", ")
      };
      store.event("launch-fresh", &format!("{name} (idle: {idle}): {reason}"))?;
    }
  }

  let started = runtime.start(StartSession {
    id: name,
    run_dir: &store.run_dir,
    kind: options.kind,
  })?;
  let external_session_id = started.external_id;
  let pane_id = started.pane_id;
  let tab_id = started.tab_id;
  let launched_head = git_stdout(store, &["rev-parse", "HEAD"]).ok();
  let transaction = store.write_transaction()?;
  session::stop_named(&transaction, name)?;
  session::create(
    &transaction,
    name,
    options.role,
    &external_session_id,
    launched_head.as_deref(),
  )?;
  transaction.commit()?;
  store.event("launch", name)?;
  println!(
    "{}",
    json!({"name": name, "pane_id": pane_id, "tab_id": tab_id, "session_id": external_session_id})
  );
  Ok(())
}

fn cmd_prompt(
  store: &Store,
  runtime: &dyn SessionRuntime,
  name: &str,
  text: &str,
  wait: bool,
  timeout: u64,
) -> Result<()> {
  let lock_path = PathBuf::from(format!("{}.prompt-lock", store.path.display()));
  let lock = OpenOptions::new()
    .create(true)
    .write(true)
    .truncate(false)
    .open(lock_path)?;
  lock.lock_exclusive()?;
  let needle: String = text.chars().take(80).collect();
  store.db.execute(
    "insert into prompts(session,text,sent_at,attempts) values(?,?,?,0)",
    params![name, text, now()],
  )?;
  let prompt_id = store.db.last_insert_rowid();
  let prompt_landing_millis = Settings::load(&store.run_dir)?.prompt_landing_seconds() * 1000;

  for attempt in 1..=PROMPT_ATTEMPTS {
    // Polling the runtime gives it a turn to deliver what a busy session has
    // queued; the landing check below then reads what actually arrived.
    let _ = runtime.query(name);
    let path_before = session_log_named(store, name)?;
    let mut offset = file_size(path_before.as_deref());
    store.db.execute(
      "update prompts set attempts=attempts+1 where id=?",
      [prompt_id],
    )?;
    let _ = runtime.prompt(name, text);
    let deadline = now() + prompt_landing_millis;
    while now() < deadline {
      let _ = runtime.query(name);
      let path = session_log_named(store, name)?;
      if path != path_before {
        offset = 0;
      }
      if let Some(path) = path
        && let Some(landing) = prompt_landed(&path, offset, &needle)
      {
        store.db.execute(
          "update prompts set landed_at=? where id=?",
          params![now(), prompt_id],
        )?;
        if landing == PromptLanding::Queued {
          store.event("prompt-queued", name)?;
        }
        FileExt::unlock(&lock)?;
        if wait {
          let _ = runtime.wait(name, Duration::from_secs(timeout));
          println!(
            "{}",
            latest_assistant_text(&path).unwrap_or_else(|| "(no assistant text)".to_owned())
          );
        }
        return Ok(());
      }
      thread::sleep(Duration::from_secs(1));
    }
    eprintln!("prompt did not land (attempt {attempt}), resending");
  }
  store.event("prompt-failed", name)?;
  FileExt::unlock(&lock)?;
  bail!("supervisor: prompt to {name} never landed after {PROMPT_ATTEMPTS} attempts")
}

fn cmd_start_commentator(
  store: &Store,
  runtime: &dyn SessionRuntime,
  role_prompt: &Path,
) -> Result<()> {
  let name = commentator_agent_name(&store.run_dir);
  cmd_launch(
    store,
    runtime,
    &name,
    LaunchOptions {
      role: Role::Commentator,
      kind: SessionKind::Commentator,
      fresh: false,
      reason: None,
    },
  )?;
  store.set_cfg("commentator", &name)?;
  let role_prompt = absolute_path(role_prompt)?;
  cmd_prompt(
    store,
    runtime,
    &name,
    &format!(
      "Read and follow this role prompt entirely: {}\nSession-log directory: {}\nRun directory: {}",
      role_prompt.display(),
      store.logs_dir.display(),
      store.run_dir.display()
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
  let mut hasher = Sha1::new();
  hasher.update(run_dir.to_string_lossy().as_bytes());
  format!("commentator-{:x}", hasher.finalize())[..20].to_owned()
}

fn task_snapshot(store: &Store, task_id: i64) -> Result<Option<Task>> {
  let transaction = store.db.unchecked_transaction()?;
  let task = task::get(&transaction, task_id)?;
  transaction.commit()?;
  Ok(task)
}

fn task_snapshots_for_session(store: &Store, session_id: i64) -> Result<Vec<Task>> {
  let transaction = store.db.unchecked_transaction()?;
  let tasks = task::tasks_for_session(&transaction, session_id)?;
  transaction.commit()?;
  Ok(tasks)
}

fn cmd_task_new(
  store: &Store,
  runtime: &dyn SessionRuntime,
  mut predicted_files: Option<i64>,
  predicted_lines: i64,
  retry_of_task_id: Option<i64>,
  files: Option<&str>,
  reason: Option<&str>,
) -> Result<()> {
  let mut text = String::new();
  std::io::stdin().read_to_string(&mut text)?;
  if text.trim().is_empty() {
    bail!("supervisor: task text on stdin is empty");
  }
  let active_retry = match retry_of_task_id {
    Some(retry_of_task_id) => {
      let predecessor = task_snapshot(store, retry_of_task_id)?.with_context(|| {
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
    abort_task(store, runtime, retry_of_task_id, reason)?;
  }
  let transaction = store.write_transaction()?;
  let task = task::create(
    &transaction,
    &text,
    predicted_files,
    predicted_lines,
    retry_of_task_id,
    predicted_file_list,
  )?;
  transaction.commit()?;
  println!("{}", task.id());
  Ok(())
}

fn reuse_preamble(store: &Store, task: &Task, session: &Session) -> Result<String> {
  let Some(since) = last_seen_commit(store, session.id())? else {
    return Ok(String::new());
  };
  let (commits, _, _) = staleness(store, Some(&since))?;
  if commits == 0 {
    return Ok(String::new());
  }
  let range = format!("{since}..HEAD");
  let log = git_stdout(store, &["log", "--oneline", "--no-decorate", &range])?;
  let changed = git_stdout(store, &["diff", "--name-only", &since, "HEAD"])?
    .lines()
    .map(str::to_owned)
    .collect::<Vec<_>>();
  let own: HashSet<_> = task
    .predicted_file_list()
    .unwrap_or_default()
    .iter()
    .map(String::as_str)
    .collect();
  let outside = changed
    .iter()
    .filter(|file| !own.contains(file.as_str()))
    .cloned()
    .collect::<Vec<_>>();
  let plural = if commits == 1 { "" } else { "s" };
  let mut lines = vec![
    format!(
      "Since your last turn (your commit {}), {commits} commit{plural} landed:",
      short_sha(&since)
    ),
    log,
  ];
  if own.is_empty() {
    lines.push(format!(
      "They touched: {} (this task carries no file list, so this is unfiltered).",
      if changed.is_empty() {
        "nothing".to_owned()
      } else {
        changed.join(", ")
      }
    ));
  } else {
    lines.push(format!(
            "Outside this task's files they touched: {}. Files in your task are not listed here; read them fresh as you open them.",
            if outside.is_empty() {
                "nothing".to_owned()
            } else {
                outside.join(", ")
            }
        ));
  }
  Ok(format!("{}\n\n", lines.join("\n")))
}

fn cmd_dispatch(
  store: &Store,
  runtime: &dyn SessionRuntime,
  task_id: i64,
  implementer: &str,
  reuse: bool,
  reason: Option<&str>,
) -> Result<()> {
  let Some(task) = task_snapshot(store, task_id)? else {
    bail!("supervisor: task {task_id} is not in state drafted");
  };
  if task.state() != TaskState::Drafted {
    bail!("supervisor: task {task_id} is not in state drafted");
  }
  let transaction = store.db.unchecked_transaction()?;
  let flying = task::all(&transaction)?
    .into_iter()
    .find(|task| matches!(task.state(), TaskState::Dispatched | TaskState::InFlight));
  transaction.commit()?;
  if let Some(flying) = flying {
    bail!(
      "supervisor: an implementer is already in flight ({} is in flight on task {})",
      session_name(store, flying.session_id())?,
      flying.id()
    );
  }
  let Some(session) = latest_session_named(store, implementer)? else {
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
  let prior = last_task_on(store, session.id())?;
  if let Some(prior) = &prior
    && !reuse
  {
    bail!(
      "supervisor: {implementer} already took task {} ({}); dispatching to it again is a reuse: pass --reuse, or launch a fresh implementer",
      prior.id(),
      prior.state()
    );
  }
  if reuse && prior.is_none() {
    bail!("supervisor: {implementer} has never taken a task; dispatch without --reuse");
  }
  if reuse && let Some(problem) = reuse_verdict(store, &session)? {
    bail!("supervisor: cannot reuse {implementer}: {problem}");
  }

  let preamble = if reuse {
    reuse_preamble(store, &task, &session)?
  } else {
    files_changed_since_launch(store, &session)?
  };

  let prompt = format!(
    "{}{text}\n\n{CONTRACT}",
    preamble,
    text = task.text().trim_end()
  );
  // The task is only dispatched once the prompt has landed in the session log,
  // so a send that never lands leaves it drafted and dispatchable again.
  if let Err(error) = cmd_prompt(store, runtime, implementer, &prompt, false, 300) {
    store.event(
      "dispatch-failed",
      &format!("task {task_id} -> {implementer}: prompt never landed"),
    )?;
    return Err(error);
  }
  let dispatch_reason = reason
    .map(str::to_owned)
    .or_else(|| reuse.then(|| format!("reuse of {implementer}")));
  let log_offset = file_size(session_log(store, &session).as_deref());
  let transaction = store.write_transaction()?;
  task::dispatch(
    &transaction,
    task_id,
    session.id(),
    log_offset as i64,
    reuse,
    dispatch_reason.as_deref(),
  )?;
  transaction.commit()?;
  store.event("dispatch", &format!("task {task_id} -> {implementer}"))?;
  let reuse_note = if reuse { " (reuse)" } else { "" };
  println!(
    "task {task_id} dispatched to {implementer}{reuse_note}; watch `state --task {task_id}` — it prints `{task_id} committed_unverified` when the commit lands"
  );
  Ok(())
}

/// The session may have read the tree before its task arrived; name what moved
/// since it started so it rereads that first. Empty when nothing has.
fn files_changed_since_launch(store: &Store, session: &Session) -> Result<String> {
  let Some(head) = session.launched_head() else {
    return Ok(String::new());
  };
  let files = git_stdout(store, &["diff", "--name-only", &format!("{head}..HEAD")])?;
  if files.is_empty() {
    return Ok(String::new());
  }
  Ok(format!(
    "These files changed since your session started; read them first: {}\n\n",
    files.lines().collect::<Vec<_>>().join(", ")
  ))
}

fn short_sha(sha: &str) -> &str {
  sha.get(..10).unwrap_or(sha)
}

fn new_commit_for(
  store: &Store,
  shas: &[String],
  base_head: Option<&str>,
) -> Result<Option<String>> {
  let Some(base_head) = base_head else {
    return Ok(None);
  };
  for sha in shas.iter().rev() {
    if base_head.starts_with(sha) {
      continue;
    }
    if !git(store, &["cat-file", "-e", sha])?.status.success() {
      continue;
    }
    if !git(store, &["merge-base", "--is-ancestor", base_head, sha])?
      .status
      .success()
    {
      continue;
    }
    return Ok(Some(sha.clone()));
  }
  Ok(None)
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

fn canonical_commit(store: &Store, sha: &str) -> Result<Option<String>> {
  if !(7..=40).contains(&sha.len())
    || !sha
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Ok(None);
  }
  let revision = format!("{sha}^{{commit}}");
  let output = git(store, &["rev-parse", "--verify", &revision])?;
  if !output.status.success() {
    return Ok(None);
  }
  Ok(Some(
    String::from_utf8_lossy(&output.stdout).trim().to_owned(),
  ))
}

fn cmd_task_record_commit(
  store: &Store,
  task_id: i64,
  sha: &str,
  force: bool,
  reason: Option<&str>,
) -> Result<()> {
  let Some(task) = task_snapshot(store, task_id)? else {
    bail!("supervisor: no task {task_id}");
  };
  let reason = forced_remedy_reason("task record-commit", force, reason)?;
  if !matches!(task.state(), TaskState::Dispatched | TaskState::InFlight) {
    bail!(
      "supervisor: task {task_id} is {}, not awaiting a coordinator-recorded commit",
      task.state()
    );
  }
  let Some(commit_sha) = canonical_commit(store, sha)? else {
    bail!("supervisor: commit {sha} does not exist in the run repository");
  };
  let transaction = store.write_transaction()?;
  for other in task::all(&transaction)? {
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
  if commit_sha == canonical_commit(store, base_head)?.unwrap_or_default()
    || !git(
      store,
      &["merge-base", "--is-ancestor", base_head, &commit_sha],
    )?
    .status
    .success()
  {
    bail!(
      "supervisor: commit {sha} does not descend from task {task_id}'s base_head as a new commit"
    );
  }
  task::record_commit(&transaction, task_id, sha, Some(reason))?;
  transaction.commit()?;
  store.event("forced-commit", &format!("task {task_id} {sha}: {reason}"))?;
  println!("task {task_id} commit recorded by force: {sha}");
  Ok(())
}

fn cmd_task_record_commentary(
  store: &Store,
  task_id: i64,
  force: bool,
  reason: Option<&str>,
) -> Result<()> {
  let Some(task) = task_snapshot(store, task_id)? else {
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
  let transaction = store.write_transaction()?;
  if !commentary_delivery::record(&transaction, task_id)? {
    bail!("supervisor: commentary delivery is already recorded for task {task_id}");
  }
  transaction.commit()?;
  store.event("forced-commentary", &format!("task {task_id}: {reason}"))?;
  println!("task {task_id} commentary delivery recorded by force");
  Ok(())
}

/// Accept a task. Without `--force` this runs the mechanical gate and accepts
/// only if it passes; with it the caller's reason stands in for the gate.
fn cmd_accept(store: &Store, task_id: i64, force: bool, reason: Option<&str>) -> Result<()> {
  if task_snapshot(store, task_id)?.is_none() {
    bail!("supervisor: no task {task_id}");
  }
  match (force, reason) {
    (true, Some(reason)) => accept_without_the_gate(store, task_id, reason),
    (true, None) => bail!("supervisor: accept --force requires a non-empty --reason"),
    (false, Some(_)) => {
      bail!("supervisor: --reason only applies with --force; accept without it runs the checks")
    }
    (false, None) => accept_through_the_gate(store, task_id),
  }
}

fn accept_without_the_gate(store: &Store, task_id: i64, reason: &str) -> Result<()> {
  let Some(task) = task_snapshot(store, task_id)? else {
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
  let transaction = store.write_transaction()?;
  task::accept(&transaction, task_id, reason)?;
  transaction.commit()?;
  store.event("accepted", &format!("task {task_id}: {reason}"))?;
  println!("task {task_id} accepted without the gate: {reason}");
  Ok(())
}

fn accept_through_the_gate(store: &Store, task_id: i64) -> Result<()> {
  let Some(task) = task_snapshot(store, task_id)? else {
    bail!("supervisor: no task {task_id}");
  };
  let mut log = match task.session_id() {
    Some(session_id) => match session_snapshot(store, session_id)? {
      Some(session) => session_log(store, &session),
      None => None,
    },
    None => None,
  };
  let shas = log
    .as_deref()
    .map(|path| commits_in_log(path, task.log_offset() as u64))
    .unwrap_or_default();
  let mut sha = match task.commit_sha() {
    Some(sha) => Some(sha.to_owned()),
    None => new_commit_for(store, &shas, task.base_head())?,
  };
  if sha.is_none() && head_advanced_cleanly(store, task.base_head())? {
    thread::sleep(Duration::from_secs(VERIFY_LOG_RETRY_SECONDS));
    log = match task.session_id() {
      Some(session_id) => match session_snapshot(store, session_id)? {
        Some(session) => session_log(store, &session),
        None => None,
      },
      None => None,
    };
    let shas = log
      .as_deref()
      .map(|path| commits_in_log(path, task.log_offset() as u64))
      .unwrap_or_default();
    sha = new_commit_for(store, &shas, task.base_head())?;
  }
  let mut problems = Vec::new();
  if let Some(sha) = &sha {
    let show = git(store, &["log", "-1", "--format=%H%n%B", sha])?;
    let show_text = String::from_utf8_lossy(&show.stdout);
    if !show.status.success() {
      problems.push(format!("commit {sha} not in git"));
    } else if has_attribution_trailer(&show_text) {
      problems.push("commit carries an attribution trailer".to_owned());
    }
    let head = git_stdout(store, &["rev-parse", "HEAD"])?;
    let full_sha = show_text.lines().next().unwrap_or_default();
    if show.status.success() && !head.starts_with(full_sha) {
      problems.push("commit is not HEAD".to_owned());
    }
  } else {
    problems.push("no commit found in the implementer's log".to_owned());
  }
  if !git_stdout(store, &["status", "--porcelain"])?.is_empty() {
    problems.push("tree is dirty".to_owned());
  }
  // The implementer runs the quality gate before it commits; that is its
  // contract, and re-deriving it from the session log only costs wall time.
  if problems.is_empty() {
    let sha = sha
      .as_deref()
      .context("accepted task unexpectedly has no commit")?;
    let transaction = store.write_transaction()?;
    task::record_commit(&transaction, task_id, sha, None)?;
    task::accept(&transaction, task_id, &format!("checks passed at {sha}"))?;
    transaction.commit()?;
    println!("task {task_id} accepted: checks passed at {sha}");
    return Ok(());
  }
  println!("task {task_id} NOT accepted:");
  for problem in problems {
    println!(" - {problem}");
  }
  Err(anyhow!(""))
}

fn head_advanced_cleanly(store: &Store, base_head: Option<&str>) -> Result<bool> {
  let Some(base_head) = base_head else {
    return Ok(false);
  };
  let head = git_stdout(store, &["rev-parse", "HEAD"])?;
  if head.is_empty() || head == base_head {
    return Ok(false);
  }
  if !git(store, &["merge-base", "--is-ancestor", base_head, &head])?
    .status
    .success()
  {
    return Ok(false);
  }
  Ok(git_stdout(store, &["status", "--porcelain"])?.is_empty())
}

fn has_attribution_trailer(message: &str) -> bool {
  message.lines().any(|line| {
    let lowercase = line.to_ascii_lowercase();
    lowercase.starts_with("co-authored-by:") || lowercase.starts_with("claude-session:")
  })
}

fn failures_in_lineage(store: &Store, task_id: i64) -> Result<i64> {
  let mut failures = 0;
  let mut current = task_snapshot(store, task_id)?;
  while let Some(task) = current {
    if task.state() == TaskState::Aborted {
      failures += 1;
    }
    current = match task.retry_of_task_id() {
      Some(retry_of_task_id) => task_snapshot(store, retry_of_task_id)?,
      None => None,
    };
  }
  Ok(failures)
}

fn abort_task(
  store: &Store,
  runtime: &dyn SessionRuntime,
  task_id: i64,
  reason: &str,
) -> Result<(i64, String)> {
  let Some(task) = task_snapshot(store, task_id)? else {
    bail!("supervisor: no task {task_id}");
  };
  if reason.trim().is_empty() {
    bail!("supervisor: abort requires a non-empty --reason");
  }
  if task.state().is_terminal() {
    bail!("supervisor: task {task_id} is already {}", task.state());
  }
  let dirty = git_stdout(store, &["status", "--porcelain"])?;
  let transaction = store.write_transaction()?;
  task::abort(&transaction, task_id, reason)?;
  transaction.commit()?;
  store.event("aborted", &format!("task {task_id}: {reason}"))?;
  if let Some(session_id) = task.session_id()
    && let Some(session) = session_snapshot(store, session_id)?
    && session.is_live()
  {
    let detail = format!("task {task_id} -> {}", session.name());
    let outcome = runtime.interrupt(session.name()).and_then(|()| {
      daemon_prompt(
        store,
        runtime,
        session.name(),
        &format!(
          "supervisor: task {task_id} is aborted: {reason}. Stop, leave the tree clean, do not commit."
        ),
      )
      .then_some(())
      .with_context(|| format!("abort message did not reach {}", session.name()))
    });
    match outcome {
      Ok(()) => store.event("abort-interrupt", &detail)?,
      Err(error) => store.event("abort-unreachable", &format!("{detail}: {error}"))?,
    }
  }
  let failures = failures_in_lineage(store, task_id)?;
  Ok((failures, dirty))
}

fn cmd_abort(
  store: &Store,
  runtime: &dyn SessionRuntime,
  task_id: i64,
  reason: &str,
) -> Result<()> {
  let (failures, dirty) = abort_task(store, runtime, task_id, reason)?;
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

fn cmd_calibrate(store: &Store, task_id: i64) -> Result<()> {
  let Some(task) = task_snapshot(store, task_id)? else {
    bail!("supervisor: task {task_id} has no commit yet");
  };
  let Some(commit_sha) = task.commit_sha() else {
    bail!("supervisor: task {task_id} has no commit yet");
  };
  let stat = git_stdout(store, &["show", "--shortstat", "--format=", commit_sha])?;
  let actual_files = stat_number(&stat, "file");
  let actual_lines = stat_number(&stat, "insertion") + stat_number(&stat, "deletion");
  let dispatched_at = last_event_at(&task, |event| event.state() == TaskState::Dispatched);
  let committed_at = last_event_at(&task, |event| {
    event.state() == TaskState::CommittedUnverified
  });
  let wall = dispatched_at
    .zip(committed_at)
    .map(|(start, end)| (end - start) as f64 / 1000.0);
  let log = match task.session_id() {
    Some(session_id) => match session_snapshot(store, session_id)? {
      Some(session) => session_log(store, &session),
      None => None,
    },
    None => None,
  };
  let next_offset = match task.session_id() {
    Some(session_id) => task_snapshots_for_session(store, session_id)?
      .into_iter()
      .find(|candidate| candidate.id() > task_id && candidate.log_offset() > 0)
      .map(|candidate| candidate.log_offset() as u64),
    None => None,
  };
  let mut end = context_peak(log.as_deref(), task.log_offset() as u64, next_offset) as i64;
  if end == 0
    && let Some(session_id) = task.session_id()
  {
    end = session_snapshot(store, session_id)?.map_or(0, |session| session.context_max());
  }
  let base = task.context_size_start().unwrap_or_default();
  let context = (end - base).max(0);
  let transaction = store.write_transaction()?;
  calibration::create(
    &transaction,
    task_id,
    task.predicted_files(),
    task.predicted_lines(),
    actual_files,
    actual_lines,
    wall,
    base,
    end,
  )?;
  transaction.commit()?;
  let wall_text = wall.map_or_else(|| "None".to_owned(), |wall| (wall as i64).to_string());
  let reuse = if task.is_session_reuse() {
    ", reuse"
  } else {
    ""
  };
  println!(
    "task {task_id}: predicted {} files/{} lines, actual {actual_files} files/{actual_lines} lines, wall {wall_text}s, context {context} (session {end}, base {base}{reuse})",
    task.predicted_files(),
    task.predicted_lines()
  );
  Ok(())
}

fn cmd_observe(store: &Store, task_id: Option<i64>, text: &str) -> Result<()> {
  let transaction = store.write_transaction()?;
  if let Some(task_id) = task_id {
    require_task(&transaction, task_id)?;
  }
  let observation = observation::create(&transaction, task_id, text)?;
  transaction.commit()?;
  println!("{}", observation.id());
  Ok(())
}

fn cmd_finding(store: &Store, task_id: i64, description: &str) -> Result<()> {
  let transaction = store.write_transaction()?;
  require_task(&transaction, task_id)?;
  let finding = finding::register(&transaction, task_id, description)?;
  transaction.commit()?;
  println!("{}", finding.id());
  Ok(())
}

fn cmd_poll(store: &Store, after_observation: i64, task_id: Option<i64>) -> Result<()> {
  if after_observation < 0 {
    bail!("supervisor: --after-observation must be nonnegative");
  }
  let transaction = store.db.unchecked_transaction()?;
  if let Some(task_id) = task_id {
    require_task(&transaction, task_id)?;
  }
  let observations = observation::after(&transaction, after_observation, task_id)?;
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
  let findings = finding::unresolved(&transaction, task_id)?
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
  transaction.commit()?;
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
  store: &Store,
  finding_id: i64,
  verdict: &Verdict,
  fix_task_id: Option<i64>,
  reason: &str,
) -> Result<()> {
  let verdict = match verdict {
    Verdict::Task => FindingVerdict::Task,
    Verdict::Dropped => FindingVerdict::Dropped,
  };
  let transaction = store.write_transaction()?;
  let finding = finding::get(&transaction, finding_id)?
    .with_context(|| format!("supervisor: no finding {finding_id}"))?;
  if let Some(fix_task_id) = fix_task_id {
    require_task(&transaction, fix_task_id)?;
  }
  finding::resolve(&transaction, &finding, verdict, reason, fix_task_id)
    .map_err(|error| anyhow!("supervisor: {error}"))?;
  transaction.commit()?;
  println!("finding {finding_id} resolved");
  Ok(())
}

fn cmd_resolutions(store: &Store) -> Result<()> {
  let transaction = store.db.unchecked_transaction()?;
  let resolutions = finding::resolved(&transaction)?
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
  transaction.commit()?;
  println!("{}", json!({"resolutions": resolutions}));
  Ok(())
}

fn require_task(transaction: &rusqlite::Transaction<'_>, task_id: i64) -> Result<Task> {
  task::get(transaction, task_id)?.with_context(|| format!("supervisor: no task {task_id}"))
}

fn cmd_state(store: &Store, only_task: Option<i64>) -> Result<()> {
  store.set_cfg("last-state-read", &now().to_string())?;
  if let Some(task_id) = only_task {
    let task =
      task_snapshot(store, task_id)?.with_context(|| format!("supervisor: no task {task_id}"))?;
    println!("{task_id} {}", task.state());
    return Ok(());
  }
  println!("tasks");
  let transaction = store.db.unchecked_transaction()?;
  let tasks = task::all(&transaction)?;
  let deliveries = tasks
    .iter()
    .map(|task| {
      Ok((
        task.id(),
        commentary_delivery::delivered_at(&transaction, task.id())?,
      ))
    })
    .collect::<Result<HashMap<_, _>>>()?;
  transaction.commit()?;
  for task in tasks {
    let mut timeline = TaskState::iter()
      .filter_map(|state| {
        last_event_at(&task, |event| event.state() == state)
          .map(|at| format!("{state}@{}", clock_time(at)))
      })
      .collect::<Vec<_>>();
    if let Some(delivered_at) = deliveries.get(&task.id()).copied().flatten() {
      timeline.push(format!(
        "commentary-delivered@{}",
        clock_time(delivered_at.timestamp_millis())
      ));
    }
    let timeline = timeline.join(" ");
    let retry = task
      .retry_of_task_id()
      .map_or_else(String::new, |id| format!("  retry of {id}"));
    let reuse = if task.is_session_reuse() {
      task.context_size_start().map_or_else(
        || "  reuse (awaiting context base)".to_owned(),
        |context| format!("  reuse (context base {context})"),
      )
    } else {
      String::new()
    };
    let reason = task
      .reason()
      .map_or_else(String::new, |reason| format!("  reason: {reason}"));
    println!(
      "  {:>3} {:<10} {:<16} {:<10} {timeline}{retry}{reuse}{reason}",
      task.id(),
      task.state(),
      session_name(store, task.session_id())?,
      task.commit_sha().map(short_sha).unwrap_or("-")
    );
  }
  println!("sessions");
  for session in session_snapshots(store)? {
    let mut flags = String::new();
    let implementer = session.role() == Role::Implementer;
    if implementer && session.context() > IMPLEMENTER_LIMIT_TOKENS {
      flags.push_str(" OVER-LIMIT");
    }
    if implementer
      && last_task_on(store, session.id())?.is_some()
      && reuse_verdict(store, &session)?.is_none()
    {
      flags.push_str(" idle, reusable");
    }
    let quiet = session.quiet_seconds(Utc::now());
    if session_log(store, &session).is_some() {
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
        "  {:<16} {:<12} context UNAVAILABLE (session log not found{danger}) quiet {quiet}s{flags}",
        session.name(),
        session.role()
      );
    }
  }
  print_time_summary(store)?;
  let open_wait: Option<i64> = store
    .db
    .query_row("select 1 from human_waits where ended is null", [], |row| {
      row.get(0)
    })
    .optional()?;
  if open_wait.is_some() {
    println!("  (a human wait is open)");
  }
  let mut statement = store.db.prepare(
        "select at,kind,detail from events where kind in ('stop-lead','kick','compact','prompt-queued','prompt-failed','accepted','launch-fresh','forced-commit','forced-commentary','commentary-wake','abort-interrupt','abort-unreachable') order by at desc limit 5",
    )?;
  let events = statement.query_map([], |row| {
    Ok((
      row.get::<_, i64>(0)?,
      row.get::<_, String>(1)?,
      row.get::<_, String>(2)?,
    ))
  })?;
  for event in events {
    let (timestamp, kind, detail) = event?;
    println!("  {} {kind} {detail}", clock_time(timestamp));
  }
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

fn print_time_summary(store: &Store) -> Result<()> {
  let transaction = store.db.unchecked_transaction()?;
  let tasks = task::all(&transaction)?;
  transaction.commit()?;
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
  let mut statement = store.db.prepare("select started,ended from human_waits")?;
  let waits = statement.query_map([], |row| {
    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
  })?;
  let mut human = 0;
  for wait in waits {
    let (start, end) = wait?;
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
fn cmd_watch_transcripts(store: &Store, interval_ms: u64) -> Result<()> {
  use std::io::Write;

  let mut before = implementer_transcript_sizes(store)?;
  loop {
    std::thread::sleep(Duration::from_millis(interval_ms));
    let after = implementer_transcript_sizes(store)?;
    if let Some(line) = format_growth(&transcript_growth(&before, &after)) {
      println!("{line}");
      std::io::stdout().flush()?;
    }
    before = after;
  }
}

fn implementer_transcript_sizes(store: &Store) -> Result<BTreeMap<String, u64>> {
  let excluded: HashSet<String> = session_snapshots(store)?
    .into_iter()
    .filter(|session| session.role() != Role::Implementer)
    .map(|session| session.external_session_id().to_owned())
    .collect();
  let mut sizes = transcript_sizes(&store.logs_dir);
  sizes.retain(|name, _| !excluded.contains(name));
  Ok(sizes)
}

fn cmd_context(store: &Store, name: Option<&str>) -> Result<()> {
  for session in session_snapshots(store)?
    .into_iter()
    .filter(|session| name.is_none_or(|name| session.name() == name))
  {
    if let Some(log) = session_log(store, &session) {
      println!("{}\t{}", session.name(), context_size(Some(&log)));
    } else {
      println!("{}\tUNAVAILABLE (session log not found)", session.name());
    }
  }
  Ok(())
}

fn cmd_human_wait(store: &Store, action: HumanWaitAction) -> Result<()> {
  match action {
    HumanWaitAction::Start => {
      let open: Option<i64> = store
        .db
        .query_row("select 1 from human_waits where ended is null", [], |row| {
          row.get(0)
        })
        .optional()?;
      if open.is_none() {
        store
          .db
          .execute("insert into human_waits(started) values(?)", [now()])?;
      }
    }
    HumanWaitAction::End => {
      store.db.execute(
        "update human_waits set ended=? where ended is null",
        [now()],
      )?;
    }
  }
  Ok(())
}

fn cmd_stop(store: &Store) -> Result<()> {
  store.set_cfg("stopped", "1")?;
  store.event("stop", "run ended by the lead")?;
  println!("supervisor: stopped; the daemon will exit on its next poll");
  Ok(())
}

fn daemon_prompt(store: &Store, runtime: &dyn SessionRuntime, name: &str, text: &str) -> bool {
  match cmd_prompt(store, runtime, name, text, false, 300) {
    Ok(()) => true,
    Err(error) => {
      let _ = store.event("prompt-unreachable", &format!("{name}: {error}"));
      false
    }
  }
}

fn daemon(
  store: &Store,
  runtime: &dyn SessionRuntime,
  lead: &str,
  lead_session_id: &str,
  poll_interval: Duration,
) -> Result<()> {
  store.set_cfg("lead", lead)?;
  register_lead(store, lead, lead_session_id)?;
  store.set_cfg("stopped", "0")?;
  store.event("daemon-start", &format!("pid {}", std::process::id()))?;
  let mut sizes: HashMap<String, u64> = HashMap::new();
  let mut missing_logs = HashSet::new();
  let mut compacting = false;
  while store.cfg("stopped")?.as_deref() != Some("1") {
    let timestamp = Utc::now();
    store.set_cfg("daemon-seen", &now().to_string())?;
    for session in session_snapshots(store)?
      .into_iter()
      .filter(Session::is_live)
    {
      let name = session.name();
      let Some(log) = session_log(store, &session) else {
        if missing_logs.insert(name.to_owned()) {
          let danger = if session.role() == Role::Lead {
            "; the lead context stop threshold cannot fire"
          } else {
            ""
          };
          let detail = format!("{name} ({}): session log not found{danger}", session.role());
          eprintln!("WARNING: {detail}");
          store.event("session-log-missing", &detail)?;
        }
        continue;
      };
      if missing_logs.remove(name) {
        eprintln!(
          "supervisor: session log found for {name}: {}",
          log.display()
        );
        store.event("session-log-found", &format!("{name}: {}", log.display()))?;
      }
      let size = file_size(Some(&log));
      let context = context_size(Some(&log)) as i64;
      let grew = sizes.get(name).copied() != Some(size);
      sizes.insert(name.to_owned(), size);
      let transaction = store.write_transaction()?;
      session::record_reading(&transaction, session.id(), context, grew, timestamp)?;
      transaction.commit()?;
      let quiet = session.quiet_seconds(timestamp) as f64;

      match session.role() {
        Role::Implementer => {
          observe_implementer(store, runtime, &session, Some(&log), quiet)?;
        }
        Role::Commentator => {
          observe_commentator(
            store,
            runtime,
            &session,
            Some(&log),
            context,
            quiet,
            &mut compacting,
          )?;
        }
        Role::Lead => observe_lead(store, context)?,
      }
    }
    thread::sleep(poll_interval);
  }
  store.event("daemon-exit", "")
}

/// The lead is started by the human, so the daemon registers it from what the
/// lead says about itself. The same session id keeps its row across daemon
/// restarts; a different one is a new incarnation and stops the old row.
fn register_lead(store: &Store, lead: &str, lead_session_id: &str) -> Result<()> {
  let transaction = store.write_transaction()?;
  let current = session::latest_named(&transaction, lead)?;
  if !current.is_some_and(|session| {
    session.is_live()
      && session.role() == Role::Lead
      && session.external_session_id() == lead_session_id
  }) {
    session::stop_named(&transaction, lead)?;
    session::create(&transaction, lead, Role::Lead, lead_session_id, None)?;
  }
  transaction.commit()?;
  Ok(())
}

/// Nudge a session that has gone quiet while its runtime reports it idle. The
/// kick is latched on the session so it happens once per stall.
fn kick_if_stalled(
  store: &Store,
  runtime: &dyn SessionRuntime,
  session: &Session,
  quiet: f64,
) -> Result<()> {
  if quiet > STALE_SECONDS
    && session.can_be_kicked()
    && runtime
      .query(session.name())
      .ok()
      .flatten()
      .map(|session| session.status)
      .as_deref()
      .is_some_and(|status| matches!(status, "idle" | "done"))
    && daemon_prompt(store, runtime, session.name(), "continue")
  {
    store.event("kick", session.name())?;
    let transaction = store.write_transaction()?;
    session::record_kick(&transaction, session.id())?;
    transaction.commit()?;
  }
  Ok(())
}

fn observe_implementer(
  store: &Store,
  runtime: &dyn SessionRuntime,
  session: &Session,
  log: Option<&Path>,
  quiet: f64,
) -> Result<()> {
  let task = task_snapshots_for_session(store, session.id())?
    .into_iter()
    .rev()
    .find(|task| matches!(task.state(), TaskState::Dispatched | TaskState::InFlight));
  let Some(task) = task else {
    return Ok(());
  };
  if task.state() == TaskState::Dispatched {
    let dispatch_offset = task.log_offset() as u64;
    if file_size(log) <= dispatch_offset {
      return Ok(());
    }
    let head = git_stdout(store, &["rev-parse", "HEAD"])?;
    let context = context_before(log, dispatch_offset);
    let transaction = store.write_transaction()?;
    task::take_flight(&transaction, task.id(), &head, context as i64)?;
    transaction.commit()?;
    return Ok(());
  }
  let shas = log
    .map(|path| commits_in_log(path, task.log_offset() as u64))
    .unwrap_or_default();
  if let Some(sha) = new_commit_for(store, &shas, task.base_head())? {
    let transaction = store.write_transaction()?;
    task::record_commit(&transaction, task.id(), &sha, None)?;
    transaction.commit()?;
    store.event("committed", &format!("task {} {sha}", task.id()))?;
  } else {
    kick_if_stalled(store, runtime, session, quiet)?;
  }
  Ok(())
}

fn observe_commentator(
  store: &Store,
  runtime: &dyn SessionRuntime,
  session: &Session,
  log: Option<&Path>,
  context: i64,
  quiet: f64,
  compacting: &mut bool,
) -> Result<()> {
  let transaction = store.db.unchecked_transaction()?;
  let mut pending = Vec::new();
  for task in task::all(&transaction)? {
    if matches!(
      task.state(),
      TaskState::CommittedUnverified | TaskState::Accepted
    ) && task.commit_sha().is_some()
      && commentary_delivery::delivered_at(&transaction, task.id())?.is_none()
    {
      let woken_at = commentary_delivery::woken_at(&transaction, task.id())?;
      pending.push((task, woken_at));
    }
  }
  transaction.commit()?;
  let text = log
    .map(|log| fs::read_to_string(log).unwrap_or_default())
    .unwrap_or_default();
  for (task, woken_at) in pending {
    let sha = task.commit_sha().unwrap_or_default();
    let abbreviation = sha.get(..7).unwrap_or(sha);
    if commentator_log_mentions(&text, abbreviation) {
      let transaction = store.write_transaction()?;
      let recorded = commentary_delivery::record(&transaction, task.id())?;
      transaction.commit()?;
      if recorded {
        store.event("commentary-delivered", &format!("task {}", task.id()))?;
      }
    } else if woken_at.is_none()
      && daemon_prompt(
        store,
        runtime,
        session.name(),
        &format!(
          "supervisor: commit {sha} landed for task {}; review it from git",
          task.id()
        ),
      )
    {
      let transaction = store.write_transaction()?;
      let recorded = commentary_delivery::record_wake(&transaction, task.id())?;
      transaction.commit()?;
      if recorded {
        store.event("commentary-wake", &format!("task {} {sha}", task.id()))?;
      }
    }
  }
  if context > COMMENTATOR_COMPACT_TOKENS && !*compacting {
    if daemon_prompt(store, runtime, session.name(), "/compact") {
      *compacting = true;
      store.event("compact", &format!("{} at {context}", session.name()))?;
    }
  } else if context < COMMENTATOR_COMPACT_TOKENS {
    *compacting = false;
  }
  kick_if_stalled(store, runtime, session, quiet)
}

fn commentator_log_mentions(text: &str, needle: &str) -> bool {
  text.lines().any(|line| {
    serde_json::from_str::<Value>(line).is_ok_and(|entry| {
      entry.get("type").and_then(Value::as_str) == Some("assistant")
        && entry.to_string().contains(needle)
    })
  })
}

/// Records the lead crossing its stop threshold once. Nothing is pushed at
/// the lead: an unsolicited prompt mid-thought is a context switch it did not
/// choose. The warning printed after every lead-facing command carries the
/// same fact at the moment the lead is already reading output.
fn observe_lead(store: &Store, context: i64) -> Result<()> {
  if context > LEAD_STOP_TOKENS && store.cfg("lead-over-limit")?.as_deref() != Some("1") {
    store.set_cfg("lead-over-limit", "1")?;
    store.event("stop-lead", &format!("context {context}"))?;
  }
  Ok(())
}
