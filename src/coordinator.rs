use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Local, TimeZone};
use fs2::FileExt;
use regex::Regex;
use rusqlite::{OptionalExtension, params};
use serde_json::json;
use sha1::{Digest, Sha1};
use strum::IntoEnumIterator;

use crate::cli::{Command, HumanWaitAction, TaskCommand, Verdict};
use crate::domain::{FindingVerdict, Task, TaskState};
use crate::logs::{
  commits_in_log, context_before, context_peak, context_size, file_size, latest_assistant_text,
  prompt_landed,
};
use crate::persistence::{calibration, commentary_delivery, finding, observation, task};
use crate::session_runtime::{SessionKind, SessionRuntime, StartSession};
use crate::store::{Session, Store, now};

const LEAD_STOP_TOKENS: i64 = 250_000;
const COMMENTATOR_COMPACT_TOKENS: i64 = 150_000;
const IMPLEMENTER_LIMIT_TOKENS: i64 = 100_000;
const STALE_SECONDS: f64 = 600.0;
const REUSE_MAX_CONTEXT: i64 = 60_000;
const REUSE_MAX_STALE_LINES: i64 = 200;
const PROMPT_ATTEMPTS: i64 = 3;
const VERIFY_LOG_RETRY_SECONDS: u64 = 1;

const CONTRACT: &str = "Verify the tree is clean; stop if dirty. Implement only this task. Run the task's checks, then the project's quality gate last. Commit without attribution trailers, leave the tree clean, then run exactly `git log -1 --format='[chainsaw %h]'` (the supervisor reads that record), and finish with the commit id, changed-file manifest, and a one-paragraph semantic delta.";

pub fn execute(store: &Store, runtime: &dyn SessionRuntime, command: Command) -> Result<()> {
  match command {
    Command::Daemon {
      lead,
      poll_interval_ms,
    } => daemon(
      store,
      runtime,
      &lead,
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
        role: "implementer",
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
      prepopulate,
    } => {
      cmd_prompt(store, runtime, &name, &text, wait, timeout)?;
      if prepopulate && let Some(session) = store.latest_session_named(&name)? {
        store.db.execute(
          "update sessions set prepopulated_at=? where id=?",
          params![now(), session.id],
        )?;
      }
      Ok(())
    }
    Command::Task { action } => match action {
      TaskCommand::New {
        files,
        predicted_files,
        predicted_lines,
        retry_of_task_id,
      } => cmd_task_new(
        store,
        predicted_files,
        predicted_lines,
        retry_of_task_id,
        files.as_deref(),
      ),
    },
    Command::Abort { task, reason } => cmd_abort(store, task, &reason),
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
    } => cmd_accept(store, runtime, task, force, reason.as_deref()),
    Command::Calibrate { task } => cmd_calibrate(store, runtime, task),
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
    Command::State => cmd_state(store, runtime),
    Command::Context { name } => cmd_context(store, runtime, name.as_deref()),
    Command::HumanWait { action } => cmd_human_wait(store, action),
    Command::Stop => cmd_stop(store),
  }
}

struct LaunchOptions<'a> {
  role: &'a str,
  kind: SessionKind,
  fresh: bool,
  reason: Option<&'a str>,
}

fn session_log(
  store: &Store,
  runtime: &dyn SessionRuntime,
  session: &Session,
) -> Result<Option<PathBuf>> {
  let live_session_id = runtime
    .query(&session.name)
    .ok()
    .flatten()
    .map(|session| session.external_id);
  let external_session_id = if let Some(external_session_id) = live_session_id {
    if session.external_session_id.as_deref() != Some(&external_session_id) {
      store.db.execute(
        "update sessions set external_session_id=?,log_path=NULL where id=?",
        params![external_session_id, session.id],
      )?;
    }
    Some(external_session_id)
  } else {
    session.external_session_id.clone()
  };
  let Some(external_session_id) = external_session_id else {
    return Ok(None);
  };
  let expected = store.logs_dir.join(format!("{external_session_id}.jsonl"));
  let cached = if session.external_session_id.as_deref() == Some(&external_session_id) {
    session.log_path.as_deref()
  } else {
    None
  };
  let path = if expected.is_file() {
    Some(expected)
  } else if cached.is_some_and(Path::is_file) {
    cached.map(Path::to_path_buf)
  } else {
    find_session_log(store, &external_session_id)
  };
  if let Some(path) = &path {
    store.db.execute(
      "update sessions set log_path=? where id=?",
      params![path.to_string_lossy(), session.id],
    )?;
  }
  Ok(path)
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

fn session_log_named(
  store: &Store,
  runtime: &dyn SessionRuntime,
  name: &str,
) -> Result<Option<PathBuf>> {
  match store.latest_session_named(name)? {
    Some(session) => session_log(store, runtime, &session),
    None => Ok(None),
  }
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
  if session.stopped_at.is_some() {
    return Ok(Some("session is stopped".to_owned()));
  }
  let tasks = task_snapshots_for_session(store, session.id)?;
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
  let context_limit = store.cfg_i64("reuse-max-context", REUSE_MAX_CONTEXT);
  if session.context > context_limit {
    return Ok(Some(format!(
      "context {} is over reuse-max-context {context_limit}",
      session.context
    )));
  }
  let since = last.and_then(|task| task.commit_sha().or(task.base_head()).map(str::to_owned));
  let (commits, files, lines) = staleness(store, since.as_deref())?;
  let stale_limit = store.cfg_i64("reuse-max-stale-lines", REUSE_MAX_STALE_LINES);
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
  for session in store
    .sessions()?
    .into_iter()
    .filter(|session| session.role == "implementer")
  {
    if reuse_verdict(store, &session)?.is_some() {
      continue;
    }
    let since = last_seen_commit(store, session.id)?;
    pool.push(IdleSession {
      stale: staleness(store, since.as_deref())?,
      files: authored_files(store, session.id)?,
      session,
    });
  }
  Ok(pool)
}

fn describe_idle(store: &Store, idle: &IdleSession) -> Result<String> {
  let session = &idle.session;
  if last_task_on(store, session.id)?.is_none() {
    return Ok(format!(
      "{} is idle at context {} and has never taken a task — dispatch <task-id> --to {}",
      session.name, session.context, session.name
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
    session.name, session.context, session.name
  ))
}

fn cmd_launch(
  store: &Store,
  runtime: &dyn SessionRuntime,
  name: &str,
  options: LaunchOptions<'_>,
) -> Result<()> {
  if options.role == "implementer" {
    let pool: Vec<_> = idle_pool(store)?
      .into_iter()
      .filter(|idle| idle.session.name != name)
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
          .map(|item| item.session.name.as_str())
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
  store.db.execute(
    "update sessions set stopped_at=? where name=? and stopped_at is null",
    params![now(), name],
  )?;
  store.db.execute(
        "insert into sessions(name,role,pane_id,tab_id,external_session_id,started_at,last_growth) values(?,?,?,?,?,?,?)",
        params![name, options.role, pane_id, tab_id, external_session_id, now(), now()],
  )?;
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

  for attempt in 1..=PROMPT_ATTEMPTS {
    let path_before = session_log_named(store, runtime, name)?;
    let mut offset = file_size(path_before.as_deref());
    store.db.execute(
      "update prompts set attempts=attempts+1 where id=?",
      [prompt_id],
    )?;
    let _ = runtime.prompt(name, text);
    let deadline = now() + 15.0;
    while now() < deadline {
      let path = session_log_named(store, runtime, name)?;
      if path != path_before {
        offset = 0;
      }
      if let Some(path) = path
        && prompt_landed(&path, offset, &needle)
      {
        store.db.execute(
          "update prompts set landed_at=? where id=?",
          params![now(), prompt_id],
        )?;
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
      role: "commentator",
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
  mut predicted_files: Option<i64>,
  predicted_lines: i64,
  retry_of_task_id: Option<i64>,
  files: Option<&str>,
) -> Result<()> {
  let mut text = String::new();
  std::io::stdin().read_to_string(&mut text)?;
  if text.trim().is_empty() {
    bail!("supervisor: task text on stdin is empty");
  }
  if let Some(retry_of_task_id) = retry_of_task_id
    && task_snapshot(store, retry_of_task_id)?.is_none_or(|task| task.state() != TaskState::Aborted)
  {
    bail!("supervisor: --retry-of {retry_of_task_id} is not an aborted task");
  }
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
  let transaction = store.db.unchecked_transaction()?;
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
  let Some(since) = last_seen_commit(store, session.id)? else {
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
    let session_name = match flying.session_id() {
      Some(session_id) => store
        .session(session_id)?
        .map_or_else(|| "-".to_owned(), |session| session.name),
      None => "-".to_owned(),
    };
    bail!(
      "supervisor: an implementer is already in flight ({} is in flight on task {})",
      session_name,
      flying.id()
    );
  }
  let Some(session) = store.latest_session_named(implementer)? else {
    bail!("supervisor: no session {implementer}; launch it first");
  };
  let prior = last_task_on(store, session.id)?;
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
  } else if session.prepopulated_at.is_some() {
    let transaction = store.db.unchecked_transaction()?;
    let since = task::all(&transaction)?.into_iter().rev().find(|task| {
      matches!(
        task.state(),
        TaskState::CommittedUnverified | TaskState::Accepted
      ) && task.commit_sha().is_some()
    });
    transaction.commit()?;
    if let Some(sha) = since.and_then(|task| task.commit_sha().map(str::to_owned)) {
      let files = git_stdout(store, &["show", "--name-only", "--format=", &sha])?;
      format!(
        "These files changed since your reading turn; read them first: {}\n\n",
        files.lines().collect::<Vec<_>>().join(", ")
      )
    } else {
      String::new()
    }
  } else {
    String::new()
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
  let transaction = store.db.unchecked_transaction()?;
  task::dispatch(
    &transaction,
    task_id,
    session.id,
    reuse,
    dispatch_reason.as_deref(),
  )?;
  transaction.commit()?;
  store.event("dispatch", &format!("task {task_id} -> {implementer}"))?;
  let log = session_log(store, runtime, &session)?;
  let offset = file_size(log.as_deref());
  let base = context_before(log.as_deref(), offset);
  let head = git_stdout(store, &["rev-parse", "HEAD"])?;
  let transaction = store.db.unchecked_transaction()?;
  task::take_flight(&transaction, task_id, offset as i64, &head, base as i64)?;
  transaction.commit()?;
  if reuse {
    println!("task {task_id} in flight on {implementer} (reuse, context base {base})");
  } else {
    println!("task {task_id} in flight on {implementer}");
  }
  store.db.execute(
    "update sessions set prepopulated_at=NULL where id=?",
    [session.id],
  )?;
  Ok(())
}

fn short_sha(sha: &str) -> &str {
  sha.get(..10).unwrap_or(sha)
}

fn new_commit_for(
  store: &Store,
  shas: &[String],
  base_head: Option<&str>,
) -> Result<Option<String>> {
  for sha in shas.iter().rev() {
    if base_head.is_some_and(|base| base.starts_with(sha)) {
      continue;
    }
    if !git(store, &["cat-file", "-e", sha])?.status.success() {
      continue;
    }
    if let Some(base) = base_head
      && !git(store, &["merge-base", "--is-ancestor", base, sha])?
        .status
        .success()
    {
      continue;
    }
    return Ok(Some(sha.clone()));
  }
  Ok(None)
}

/// Accept a task. Without `--force` this runs the mechanical gate and accepts
/// only if it passes; with it the caller's reason stands in for the gate.
fn cmd_accept(
  store: &Store,
  runtime: &dyn SessionRuntime,
  task_id: i64,
  force: bool,
  reason: Option<&str>,
) -> Result<()> {
  if task_snapshot(store, task_id)?.is_none() {
    bail!("supervisor: no task {task_id}");
  }
  match (force, reason) {
    (true, Some(reason)) => accept_without_the_gate(store, task_id, reason),
    (true, None) => bail!("supervisor: accept --force requires a non-empty --reason"),
    (false, Some(_)) => {
      bail!("supervisor: --reason only applies with --force; accept without it runs the checks")
    }
    (false, None) => accept_through_the_gate(store, runtime, task_id),
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
  let transaction = store.db.unchecked_transaction()?;
  task::accept(&transaction, task_id, reason)?;
  transaction.commit()?;
  store.event("accepted", &format!("task {task_id}: {reason}"))?;
  println!("task {task_id} accepted without the gate: {reason}");
  Ok(())
}

fn accept_through_the_gate(
  store: &Store,
  runtime: &dyn SessionRuntime,
  task_id: i64,
) -> Result<()> {
  let Some(task) = task_snapshot(store, task_id)? else {
    bail!("supervisor: no task {task_id}");
  };
  let mut log = match task.session_id() {
    Some(session_id) => match store.session(session_id)? {
      Some(session) => session_log(store, runtime, &session)?,
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
      Some(session_id) => match store.session(session_id)? {
        Some(session) => session_log(store, runtime, &session)?,
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
    let transaction = store.db.unchecked_transaction()?;
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

fn cmd_abort(store: &Store, task_id: i64, reason: &str) -> Result<()> {
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
  let transaction = store.db.unchecked_transaction()?;
  task::abort(&transaction, task_id, reason)?;
  transaction.commit()?;
  store.event("aborted", &format!("task {task_id}: {reason}"))?;
  let failures = failures_in_lineage(store, task_id)?;
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

fn cmd_calibrate(store: &Store, runtime: &dyn SessionRuntime, task_id: i64) -> Result<()> {
  let Some(task) = task_snapshot(store, task_id)? else {
    bail!("supervisor: task {task_id} has no commit yet");
  };
  let Some(commit_sha) = task.commit_sha() else {
    bail!("supervisor: task {task_id} has no commit yet");
  };
  let stat = git_stdout(store, &["show", "--shortstat", "--format=", commit_sha])?;
  let actual_files = stat_number(&stat, "file");
  let actual_lines = stat_number(&stat, "insertion") + stat_number(&stat, "deletion");
  let dispatched_at: Option<f64> = store
    .db
    .query_row(
      "select created_at / 1000.0 from task_events
       where task_id=? and state='dispatched' order by id desc limit 1",
      [task_id],
      |row| row.get(0),
    )
    .optional()?;
  let committed_at: Option<f64> = store
    .db
    .query_row(
      "select created_at / 1000.0 from task_events
       where task_id=? and state='committed_unverified' order by id desc limit 1",
      [task_id],
      |row| row.get(0),
    )
    .optional()?;
  let wall = dispatched_at
    .zip(committed_at)
    .map(|(start, end)| end - start);
  let log = match task.session_id() {
    Some(session_id) => match store.session(session_id)? {
      Some(session) => session_log(store, runtime, &session)?,
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
    end = store
      .session(session_id)?
      .map_or(0, |session| session.context_max);
  }
  let base = task.context_size_start().unwrap_or_default();
  let context = (end - base).max(0);
  let transaction = store.db.unchecked_transaction()?;
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
  let transaction = store.db.unchecked_transaction()?;
  if let Some(task_id) = task_id {
    require_task(&transaction, task_id)?;
  }
  let observation = observation::create(&transaction, task_id, text)?;
  transaction.commit()?;
  println!("{}", observation.id());
  Ok(())
}

fn cmd_finding(store: &Store, task_id: i64, description: &str) -> Result<()> {
  let transaction = store.db.unchecked_transaction()?;
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
  let transaction = store.db.unchecked_transaction()?;
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

fn cmd_state(store: &Store, runtime: &dyn SessionRuntime) -> Result<()> {
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
    let mut statement = store
      .db
      .prepare("select state,created_at / 1000.0 from task_events where task_id=? order by id")?;
    let log = statement
      .query_map([task.id()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
      })?
      .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    let mut timeline = TaskState::iter()
      .filter_map(|state| {
        log
          .get(state.as_str())
          .map(|timestamp| format!("{state}@{}", clock_time(*timestamp)))
      })
      .collect::<Vec<_>>();
    if let Some(delivered_at) = deliveries.get(&task.id()).copied().flatten() {
      timeline.push(format!(
        "commentary-delivered@{}",
        clock_time(delivered_at as f64 / 1000.0)
      ));
    }
    let timeline = timeline.join(" ");
    let retry = task
      .retry_of_task_id()
      .map_or_else(String::new, |id| format!("  retry of {id}"));
    let reuse = if task.is_session_reuse() {
      format!(
        "  reuse (context base {})",
        task.context_size_start().unwrap_or_default()
      )
    } else {
      String::new()
    };
    let reason = task
      .reason()
      .map_or_else(String::new, |reason| format!("  reason: {reason}"));
    let session_name = match task.session_id() {
      Some(session_id) => store
        .session(session_id)?
        .map_or_else(|| "-".to_owned(), |session| session.name),
      None => "-".to_owned(),
    };
    println!(
      "  {:>3} {:<10} {:<16} {:<10} {timeline}{retry}{reuse}{reason}",
      task.id(),
      task.state(),
      session_name,
      task.commit_sha().map(short_sha).unwrap_or("-")
    );
  }
  println!("sessions");
  for session in store.sessions()? {
    let mut flags = String::new();
    if session.role == "implementer" && session.context > IMPLEMENTER_LIMIT_TOKENS {
      flags.push_str(" OVER-LIMIT");
    }
    if session.prepopulated_at.is_some() {
      flags.push_str(" pre-populated");
    }
    if session.role == "implementer"
      && last_task_on(store, session.id)?.is_some()
      && reuse_verdict(store, &session)?.is_none()
    {
      flags.push_str(" idle, reusable");
    }
    let quiet = (now() - session.last_growth.unwrap_or_else(now)) as i64;
    if session_log(store, runtime, &session)?.is_some() {
      println!(
        "  {:<16} {:<12} context {:>7} (max {}) quiet {quiet}s{flags}",
        session.name, session.role, session.context, session.context_max
      );
    } else {
      let danger = if session.role == "lead" {
        "; lead stop threshold disabled"
      } else {
        ""
      };
      println!(
        "  {:<16} {:<12} context UNAVAILABLE (session log not found{danger}) quiet {quiet}s{flags}",
        session.name, session.role
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
        "select at,kind,detail from events where kind in ('stop-lead','kick','compact','accepted','launch-fresh') order by at desc limit 5",
    )?;
  let events = statement.query_map([], |row| {
    Ok((
      row.get::<_, f64>(0)?,
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

fn clock_time(timestamp: f64) -> String {
  Local
    .timestamp_opt(timestamp as i64, 0)
    .single()
    .map_or_else(
      || "-".to_owned(),
      |time| time.format("%H:%M:%S").to_string(),
    )
}

fn print_time_summary(store: &Store) -> Result<()> {
  let first: Option<f64> = store.db.query_row(
    "select min(created_at) / 1000.0 from task_events",
    [],
    |row| row.get(0),
  )?;
  let mut busy = 0.0;
  let transaction = store.db.unchecked_transaction()?;
  let tasks = task::all(&transaction)?;
  transaction.commit()?;
  for task in tasks {
    let start: Option<f64> = store
      .db
      .query_row(
        "select created_at / 1000.0 from task_events
         where task_id=? and state='dispatched' order by id desc limit 1",
        [task.id()],
        |row| row.get(0),
      )
      .optional()?;
    let end: Option<f64> = store
            .db
            .query_row(
                "select created_at / 1000.0 from task_events where task_id=? and state in ('committed_unverified','accepted','aborted') order by id limit 1",
                [task.id()],
                |row| row.get(0),
            )
            .optional()?;
    if let Some(start) = start {
      busy += end.unwrap_or_else(now) - start;
    }
  }
  let mut statement = store.db.prepare("select started,ended from human_waits")?;
  let waits = statement.query_map([], |row| {
    Ok((row.get::<_, f64>(0)?, row.get::<_, Option<f64>>(1)?))
  })?;
  let mut human = 0.0;
  for wait in waits {
    let (start, end) = wait?;
    human += end.unwrap_or_else(now) - start;
  }
  if let Some(first) = first {
    let wall = now() - first;
    let percentage = if wall == 0.0 {
      0.0
    } else {
      100.0 * busy / wall
    };
    println!(
      "time  wall {}s  implementer-busy {}s ({percentage:.0}%)  waiting-on-human {}s",
      wall as i64, busy as i64, human as i64
    );
  }
  Ok(())
}

fn cmd_context(store: &Store, runtime: &dyn SessionRuntime, name: Option<&str>) -> Result<()> {
  for session in store
    .sessions()?
    .into_iter()
    .filter(|session| name.is_none_or(|name| session.name == name))
  {
    let log = session_log(store, runtime, &session)?;
    if let Some(log) = log {
      println!("{}\t{}", session.name, context_size(Some(&log)));
    } else {
      println!("{}\tUNAVAILABLE (session log not found)", session.name);
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
  poll_interval: Duration,
) -> Result<()> {
  store.set_cfg("lead", lead)?;
  store.db.execute(
    "insert into sessions(name,role,started_at,last_growth)
         select ?,?,?,? where not exists(
           select 1 from sessions where name=? and role='lead' and stopped_at is null
         )",
    params![lead, "lead", now(), now(), lead],
  )?;
  store.set_cfg("stopped", "0")?;
  store.event("daemon-start", &format!("pid {}", std::process::id()))?;
  let mut sizes: HashMap<String, u64> = HashMap::new();
  let mut missing_logs = HashSet::new();
  let mut compacting = false;
  while store.cfg("stopped")?.as_deref() != Some("1") {
    let timestamp = now();
    for session in store
      .sessions()?
      .into_iter()
      .filter(|session| session.stopped_at.is_none())
    {
      let name = &session.name;
      let log = session_log(store, runtime, &session)?;
      let Some(log) = log else {
        if missing_logs.insert(name.clone()) {
          let danger = if session.role == "lead" {
            "; the lead context stop threshold cannot fire"
          } else {
            ""
          };
          let detail = format!("{name} ({}): session log not found{danger}", session.role);
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
      sizes.insert(name.clone(), size);
      store.db.execute(
                "update sessions set context=?, context_max=max(context_max,?), last_growth=case when ? then ? else last_growth end, kicked_at=case when ? then null else kicked_at end where id=?",
                params![context, context, grew, timestamp, grew, session.id],
            )?;
      let quiet = timestamp - session.last_growth.unwrap_or(timestamp);

      match session.role.as_str() {
        "implementer" => {
          observe_implementer(store, runtime, &session, Some(&log), quiet)?;
        }
        "commentator" => {
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
        "lead" => observe_lead(store, runtime, name, context)?,
        _ => {}
      }
    }
    thread::sleep(poll_interval);
  }
  store.event("daemon-exit", "")
}

fn observe_implementer(
  store: &Store,
  runtime: &dyn SessionRuntime,
  session: &Session,
  log: Option<&Path>,
  quiet: f64,
) -> Result<()> {
  let task = task_snapshots_for_session(store, session.id)?
    .into_iter()
    .rev()
    .find(|task| matches!(task.state(), TaskState::Dispatched | TaskState::InFlight));
  let Some(task) = task else {
    return Ok(());
  };
  let shas = log
    .map(|path| commits_in_log(path, task.log_offset() as u64))
    .unwrap_or_default();
  if let Some(sha) = new_commit_for(store, &shas, task.base_head())? {
    let transaction = store.db.unchecked_transaction()?;
    task::record_commit(&transaction, task.id(), &sha, None)?;
    transaction.commit()?;
    store.event("committed", &format!("task {} {sha}", task.id()))?;
  } else if quiet > STALE_SECONDS
    && session.kicked_at.is_none()
    && runtime
      .query(&session.name)
      .ok()
      .flatten()
      .map(|session| session.status)
      .as_deref()
      .is_some_and(|status| matches!(status, "idle" | "done"))
  {
    store.event("kick", &session.name)?;
    store.db.execute(
      "update sessions set kicked_at=? where id=?",
      params![now(), session.id],
    )?;
    daemon_prompt(store, runtime, &session.name, "continue");
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
      pending.push(task);
    }
  }
  transaction.commit()?;
  if let Some(log) = log {
    let text = fs::read_to_string(log).unwrap_or_default();
    for task in pending {
      let sha = task.commit_sha().unwrap_or_default();
      let abbreviation = sha.get(..7).unwrap_or(sha);
      if text.contains(abbreviation) {
        let transaction = store.db.unchecked_transaction()?;
        let recorded = commentary_delivery::record(&transaction, task.id())?;
        transaction.commit()?;
        if recorded {
          store.event("commentary-delivered", &format!("task {}", task.id()))?;
        }
      }
    }
  }
  if context > COMMENTATOR_COMPACT_TOKENS && !*compacting {
    *compacting = true;
    store.event("compact", &format!("{} at {context}", session.name))?;
    daemon_prompt(store, runtime, &session.name, "/compact");
  } else if context < COMMENTATOR_COMPACT_TOKENS {
    *compacting = false;
  }
  if quiet > STALE_SECONDS
    && session.kicked_at.is_none()
    && runtime
      .query(&session.name)
      .ok()
      .flatten()
      .map(|session| session.status)
      .as_deref()
      .is_some_and(|status| matches!(status, "idle" | "done"))
  {
    store.event("kick", &session.name)?;
    store.db.execute(
      "update sessions set kicked_at=? where id=?",
      params![now(), session.id],
    )?;
    daemon_prompt(store, runtime, &session.name, "continue");
  }
  Ok(())
}

fn observe_lead(
  store: &Store,
  runtime: &dyn SessionRuntime,
  name: &str,
  context: i64,
) -> Result<()> {
  if context > LEAD_STOP_TOKENS && store.cfg("lead-told-stop")?.as_deref() != Some("1") {
    store.set_cfg("lead-told-stop", "1")?;
    store.event("stop-lead", &format!("context {context}"))?;
    daemon_prompt(
      store,
      runtime,
      name,
      &format!(
        "supervisor: your context is {context} tokens, past {LEAD_STOP_TOKENS}. Stop the run per the skill's Stopping section: the human must ask them plainly whether to end the run for good, then let the in-flight implementer finish, wait for the commentator on that commit, write the continuation prompt, then stop."
      ),
    );
  }
  Ok(())
}
