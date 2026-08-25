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
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::cli::{Command, HumanWaitAction, TaskCommand, Verdict};
use crate::domain::TaskState;
use crate::logs::{
  bash_commands, commits_in_log, context_before, context_peak, context_size, file_size,
  latest_assistant_text, prompt_landed,
};
use crate::persistence::{calibration, task_event};
use crate::store::{Session, Store, TASK_STATES, Task, now};

const LEAD_STOP_TOKENS: i64 = 250_000;
const COMMENTATOR_COMPACT_TOKENS: i64 = 150_000;
const IMPLEMENTER_LIMIT_TOKENS: i64 = 100_000;
const STALE_SECONDS: f64 = 600.0;
const POLL_SECONDS: u64 = 5;
const REUSE_MAX_CONTEXT: i64 = 60_000;
const REUSE_MAX_STALE_LINES: i64 = 200;
const PROMPT_ATTEMPTS: i64 = 3;
const VERIFY_LOG_RETRY_SECONDS: u64 = 1;

const CONTRACT: &str = "Verify the tree is clean; stop if dirty. Implement only this task. Run the task's checks, then the project's quality gate last. Commit without attribution trailers, leave the tree clean, then run exactly `git log -1 --format='[chainsaw %h]'` (the supervisor reads that record), and finish with the commit id, changed-file manifest, and a one-paragraph semantic delta.";

const IMPLEMENTER_FLAGS: &[&str] = &[
  "--model",
  "opus",
  "--effort",
  "high",
  "--disable-slash-commands",
  "--strict-mcp-config",
  "--no-chrome",
  "--disallowedTools",
  "WebSearch,WebFetch,NotebookEdit,Task,Agent,AskUserQuestion,EnterPlanMode,ExitPlanMode,TaskOutput",
];

const COMMENTATOR_FLAGS: &[&str] = &[
  "--model",
  "opus",
  "--effort",
  "high",
  "--strict-mcp-config",
  "--no-chrome",
  "--disallowedTools",
  "WebSearch,WebFetch,NotebookEdit,Task,Agent,AskUserQuestion,EnterPlanMode,ExitPlanMode,TaskOutput",
];

pub fn execute(store: &Store, command: Command) -> Result<()> {
  match command {
    Command::Daemon { lead } => daemon(store, &lead),
    Command::StartCommentator { role_prompt } => cmd_start_commentator(store, &role_prompt),
    Command::Launch {
      name,
      fresh,
      reason,
    } => cmd_launch(
      store,
      &name,
      LaunchOptions {
        role: "implementer",
        flags: IMPLEMENTER_FLAGS,
        split: false,
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
      cmd_prompt(store, &name, &text, wait, timeout)?;
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
    Command::Fail { task, reason } => cmd_fail(store, task, &reason),
    Command::Dispatch { task, to, reuse } => cmd_dispatch(store, task, &to, reuse),
    Command::Verify { task } => cmd_verify(store, task),
    Command::Accept { task, reason } => cmd_accept(store, task, &reason),
    Command::Calibrate { task } => cmd_calibrate(store, task),
    Command::Disposition {
      finding,
      verdict,
      task,
      reason,
    } => cmd_disposition(store, &finding, &verdict, task, &reason),
    Command::Config { key, value } => {
      if let Some(value) = value {
        store.set_cfg(&key, &value)
      } else {
        println!("{}", store.cfg_or(&key, "")?);
        Ok(())
      }
    }
    Command::State => cmd_state(store),
    Command::Comments { all } => cmd_comments(store, all),
    Command::Context { name } => cmd_context(store, name.as_deref()),
    Command::HumanWait { action } => cmd_human_wait(store, action),
    Command::Stop => cmd_stop(store),
  }
}

struct LaunchOptions<'a> {
  role: &'a str,
  flags: &'a [&'a str],
  split: bool,
  fresh: bool,
  reason: Option<&'a str>,
}

fn run(program: &str, args: &[&str]) -> Result<Output> {
  ProcessCommand::new(program)
    .args(args)
    .output()
    .with_context(|| format!("failed to run {program}"))
}

fn herdr(args: &[&str]) -> Result<Value> {
  let output = run("herdr", args)?;
  if !output.status.success() {
    bail!(
      "herdr failed: {}",
      String::from_utf8_lossy(&output.stderr).trim()
    );
  }
  serde_json::from_slice(&output.stdout).context("herdr returned invalid JSON")
}

fn herdr_session_id(name: &str) -> Option<String> {
  herdr(&["agent", "get", name])
    .ok()?
    .pointer("/result/agent/agent_session/value")?
    .as_str()
    .map(str::to_owned)
}

fn herdr_status(name: &str) -> Option<String> {
  herdr(&["agent", "get", name])
    .ok()?
    .pointer("/result/agent/status")?
    .as_str()
    .map(str::to_owned)
}

fn session_log(store: &Store, session: &Session) -> Result<Option<PathBuf>> {
  let live_session_id = herdr_session_id(&session.name);
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

fn session_log_named(store: &Store, name: &str) -> Result<Option<PathBuf>> {
  match store.latest_session_named(name)? {
    Some(session) => session_log(store, &session),
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
    store
      .tasks()?
      .into_iter()
      .rev()
      .find(|task| task.session_id == Some(session_id) && task.state != "drafted"),
  )
}

fn last_seen_commit(store: &Store, session_id: i64) -> Result<Option<String>> {
  Ok(last_task_on(store, session_id)?.and_then(|task| task.commit_sha.or(task.base_head)))
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
  for task in store
    .tasks()?
    .into_iter()
    .filter(|task| task.session_id == Some(session_id) && task.commit_sha.is_some())
  {
    let sha = task.commit_sha.as_deref().unwrap_or_default();
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
  if store.tasks()?.iter().any(|task| {
    task.session_id == Some(session.id) && matches!(task.state.as_str(), "dispatched" | "in_flight")
  }) {
    return Ok(Some("session is in flight".to_owned()));
  }
  let last = last_task_on(store, session.id)?;
  if let Some(task) = &last
    && task.state == "failed"
  {
    return Ok(Some(format!(
      "its last task ({}) failed; a retry gets a fresh head",
      task.id
    )));
  }
  let context_limit = store.cfg_i64("reuse-max-context", REUSE_MAX_CONTEXT);
  if session.context > context_limit {
    return Ok(Some(format!(
      "context {} is over reuse-max-context {context_limit}",
      session.context
    )));
  }
  let since = last.and_then(|task| task.commit_sha.or(task.base_head));
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

fn cmd_launch(store: &Store, name: &str, options: LaunchOptions<'_>) -> Result<()> {
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

  let workspace = env::var("HERDR_WORKSPACE_ID")
    .map_err(|_| anyhow!("supervisor: must run inside a Herdr pane"))?;
  let (pane_id, tab_id) = if options.split {
    let run_dir = store.run_dir.to_string_lossy();
    let response = herdr(&[
      "pane",
      "split",
      "--current",
      "--direction",
      "right",
      "--cwd",
      &run_dir,
      "--no-focus",
    ])?;
    (
      json_string(&response, "/result/pane/pane_id")?,
      env::var("HERDR_TAB_ID").unwrap_or_default(),
    )
  } else {
    let run_dir = store.run_dir.to_string_lossy();
    let response = herdr(&[
      "tab",
      "create",
      "--workspace",
      &workspace,
      "--label",
      name,
      "--cwd",
      &run_dir,
      "--no-focus",
    ])?;
    (
      json_string(&response, "/result/root_pane/pane_id")?,
      json_string(&response, "/result/tab/tab_id")?,
    )
  };

  let mut start_args = vec!["agent", "start", name, "--kind", "claude", "--pane"];
  start_args.push(&pane_id);
  start_args.push("--");
  start_args.extend_from_slice(options.flags);
  let mut started = None;
  for attempt in 0..5 {
    match herdr(&start_args) {
      Ok(response) => {
        started = Some(response);
        break;
      }
      Err(error) if attempt == 4 => return Err(error),
      Err(_) => thread::sleep(Duration::from_secs(2)),
    }
  }
  let started = started.context("herdr agent did not start")?;
  let external_session_id = json_string(&started, "/result/agent/agent_session/value")?;
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

fn json_string(value: &Value, pointer: &str) -> Result<String> {
  value
    .pointer(pointer)
    .and_then(Value::as_str)
    .map(str::to_owned)
    .with_context(|| format!("herdr response lacks {pointer}"))
}

fn cmd_prompt(store: &Store, name: &str, text: &str, wait: bool, timeout: u64) -> Result<()> {
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
    let path_before = session_log_named(store, name)?;
    let mut offset = file_size(path_before.as_deref());
    store.db.execute(
      "update prompts set attempts=attempts+1 where id=?",
      [prompt_id],
    )?;
    let _ = run("herdr", &["agent", "prompt", name, text]);
    let deadline = now() + 15.0;
    while now() < deadline {
      let path = session_log_named(store, name)?;
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
          let timeout_ms = (timeout * 1000).to_string();
          let _ = run("herdr", &["agent", "wait", name, "--timeout", &timeout_ms]);
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

fn cmd_start_commentator(store: &Store, role_prompt: &Path) -> Result<()> {
  let name = commentator_agent_name(&store.run_dir);
  cmd_launch(
    store,
    &name,
    LaunchOptions {
      role: "commentator",
      flags: COMMENTATOR_FLAGS,
      split: true,
      fresh: false,
      reason: None,
    },
  )?;
  store.set_cfg("commentator", &name)?;
  let role_prompt = absolute_path(role_prompt)?;
  cmd_prompt(
    store,
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
    && store
      .task(retry_of_task_id)?
      .is_none_or(|task| task.state != "failed")
  {
    bail!("supervisor: --retry-of {retry_of_task_id} is not a failed task");
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
  let predicted_file_list = (!file_list.is_empty()).then(|| file_list.join(","));
  store.db.execute(
        "insert into tasks(text,predicted_files,predicted_lines,state,created_at,retry_of_task_id,predicted_file_list) values(?,?,?,?,?,?,?)",
        params![text, predicted_files, predicted_lines, "drafted", now(), retry_of_task_id, predicted_file_list],
    )?;
  let task_id = store.db.last_insert_rowid();
  store.set_task_state(task_id, TaskState::Drafted)?;
  println!("{task_id}");
  Ok(())
}

fn predecessor_unverified(store: &Store, task_id: i64) -> Result<Option<String>> {
  let previous = store.tasks()?.into_iter().rfind(|task| task.id < task_id);
  let Some(previous) = previous else {
    return Ok(None);
  };
  if matches!(
    previous.state.as_str(),
    "drafted" | "dispatched" | "in_flight" | "failed"
  ) {
    return Ok(None);
  }
  let mut statement = store
    .db
    .prepare("select state from task_events where task_id=?")?;
  let states = statement
    .query_map([previous.id], |row| row.get::<_, String>(0))?
    .collect::<rusqlite::Result<HashSet<_>>>()?;
  if states.contains("verified") || states.contains("accepted") {
    return Ok(None);
  }
  Ok(Some(format!(
    "task {} is {} but never verified; run verify {}, or accept {} --reason ... if the failure is a false positive",
    previous.id, previous.state, previous.id, previous.id
  )))
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
    .predicted_file_list
    .as_deref()
    .unwrap_or_default()
    .split(',')
    .filter(|file| !file.is_empty())
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

fn cmd_dispatch(store: &Store, task_id: i64, implementer: &str, reuse: bool) -> Result<()> {
  let Some(task) = store.task(task_id)? else {
    bail!("supervisor: task {task_id} is not in state drafted");
  };
  if task.state != "drafted" {
    bail!("supervisor: task {task_id} is not in state drafted");
  }
  if let Some(flying) = store
    .tasks()?
    .into_iter()
    .find(|task| matches!(task.state.as_str(), "dispatched" | "in_flight"))
  {
    let session_name = match flying.session_id {
      Some(session_id) => store
        .session(session_id)?
        .map_or_else(|| "-".to_owned(), |session| session.name),
      None => "-".to_owned(),
    };
    bail!(
      "supervisor: an implementer is already in flight ({} is in flight on task {})",
      session_name,
      flying.id
    );
  }
  if let Some(problem) = predecessor_unverified(store, task_id)? {
    bail!("supervisor: {problem}");
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
      prior.id,
      prior.state
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
    let since = store.tasks()?.into_iter().rev().find(|task| {
      matches!(
        task.state.as_str(),
        "committed" | "verified" | "accepted" | "ingested"
      ) && task.commit_sha.is_some()
    });
    if let Some(sha) = since.and_then(|task| task.commit_sha) {
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

  store.db.execute(
    "update tasks set session_id=?, is_session_reuse=? where id=?",
    params![session.id, i64::from(reuse), task_id],
  )?;
  store.set_task_state(task_id, TaskState::Dispatched)?;
  store.event("dispatch", &format!("task {task_id} -> {implementer}"))?;
  let prompt = format!(
    "{}{text}\n\n{CONTRACT}",
    preamble,
    text = task.text.trim_end()
  );
  if let Err(error) = cmd_prompt(store, implementer, &prompt, false, 300) {
    store.set_task_state(task_id, TaskState::Drafted)?;
    store.event(
      "dispatch-failed",
      &format!("task {task_id} -> {implementer}: prompt never landed"),
    )?;
    return Err(error);
  }
  let log = session_log(store, &session)?;
  let offset = file_size(log.as_deref());
  let base = context_before(log.as_deref(), offset);
  let head = git_stdout(store, &["rev-parse", "HEAD"])?;
  store.db.execute(
    "update tasks set log_offset=?, base_head=?, context_size_start=? where id=?",
    params![offset as i64, head, base as i64, task_id],
  )?;
  store.set_task_state(task_id, TaskState::InFlight)?;
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

fn cmd_verify(store: &Store, task_id: i64) -> Result<()> {
  let Some(task) = store.task(task_id)? else {
    bail!("supervisor: no task {task_id}");
  };
  let mut log = match task.session_id {
    Some(session_id) => match store.session(session_id)? {
      Some(session) => session_log(store, &session)?,
      None => None,
    },
    None => None,
  };
  let shas = log
    .as_deref()
    .map(|path| commits_in_log(path, task.log_offset as u64))
    .unwrap_or_default();
  let mut sha = match &task.commit_sha {
    Some(sha) => Some(sha.clone()),
    None => new_commit_for(store, &shas, task.base_head.as_deref())?,
  };
  if sha.is_none() && head_advanced_cleanly(store, task.base_head.as_deref())? {
    thread::sleep(Duration::from_secs(VERIFY_LOG_RETRY_SECONDS));
    log = match task.session_id {
      Some(session_id) => match store.session(session_id)? {
        Some(session) => session_log(store, &session)?,
        None => None,
      },
      None => None,
    };
    let shas = log
      .as_deref()
      .map(|path| commits_in_log(path, task.log_offset as u64))
      .unwrap_or_default();
    sha = new_commit_for(store, &shas, task.base_head.as_deref())?;
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
  match (store.cfg("gate")?, log.as_deref()) {
    (Some(gate), Some(log)) => {
      if let Some(problem) = gate_last_problem(
        &bash_commands(log, task.log_offset as u64),
        &gate,
        &store.run_dir,
      ) {
        problems.push(problem);
      }
    }
    (None, _) => problems
      .push("gate not configured (chainsaw config gate CMD): gate-last unchecked".to_owned()),
    _ => {}
  }
  if let Some(sha) = &sha
    && task.commit_sha.is_none()
  {
    store.db.execute(
      "update tasks set commit_sha=? where id=?",
      params![sha, task_id],
    )?;
  }
  let has_hard_problem = problems
    .iter()
    .any(|problem| !problem.starts_with("gate not configured"));
  if !has_hard_problem {
    store.set_task_state(task_id, TaskState::Verified)?;
    println!(
      "task {task_id} verified: {}",
      sha.as_deref().unwrap_or_default()
    );
    for problem in problems {
      println!("note: {problem}");
    }
    return Ok(());
  }
  println!("task {task_id} NOT verified:");
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

fn gate_last_problem(commands: &[(String, bool)], gate: &str, run_dir: &Path) -> Option<String> {
  let commit_index = commands
    .iter()
    .rposition(|(command, _)| command.contains("git commit"));
  let Some(commit_index) = commit_index else {
    return Some(format!(
      "no git commit in the log; gate-last unchecked ({gate:?})"
    ));
  };
  let gate_index = commands[..commit_index]
    .iter()
    .rposition(|(command, _)| contains_gate(command, gate));
  let Some(gate_index) = gate_index else {
    return Some(format!("the gate ({gate:?}) did not run before the commit"));
  };
  if !commands[gate_index].1 {
    return Some("the gate before the commit failed (error result in the log)".to_owned());
  }
  let mut intervening = commands[gate_index + 1..commit_index].to_vec();
  if let Some(before_commit) = before_git_commit(&commands[commit_index].0)
    && !before_commit.trim().is_empty()
  {
    intervening.push((before_commit.trim().to_owned(), true));
  }
  for (command, ok) in intervening {
    if !ok {
      let first_line = command.lines().next().unwrap_or_default();
      return Some(format!(
        "a command after the gate failed: {:?}",
        truncate(first_line, 80)
      ));
    }
    if !command_harmless_after_gate(&command, run_dir) {
      return Some(format!(
        "source-modifying command after the gate: {:?}",
        truncate(command.lines().next().unwrap_or_default(), 80)
      ));
    }
  }
  None
}

fn contains_gate(command: &str, gate: &str) -> bool {
  let actual = command_backbone(command);
  let wanted = command_backbone(gate);
  !wanted.is_empty()
    && actual.match_indices(&wanted).any(|(start, matched)| {
      let end = start + matched.len();
      shell_boundary_before(&actual[..start]) && shell_boundary_after(&actual[end..])
    })
}

fn shell_boundary_before(prefix: &str) -> bool {
  prefix
    .trim_end()
    .chars()
    .next_back()
    .is_none_or(shell_boundary)
}

fn shell_boundary_after(suffix: &str) -> bool {
  suffix
    .trim_start()
    .chars()
    .next()
    .is_none_or(shell_boundary)
}

fn shell_boundary(character: char) -> bool {
  matches!(character, ';' | '|' | '&' | '(' | ')' | '{' | '}')
}

fn command_backbone(command: &str) -> String {
  let redirect = Regex::new(r#"(?:\d+\s*)?(?:>&|<&|>>|<<|>|<)\s*(?:"[^"]*"|'[^']*'|[^\s;|&]+)"#)
    .expect("valid redirect regex");
  let separators = Regex::new(r"\s*([;|&]+)\s*").expect("valid separator regex");
  let without_heredocs = without_heredoc_bodies(command).replace('\n', ";");
  let without_redirects = redirect.replace_all(&without_heredocs, "");
  separators
    .replace_all(&without_redirects, "$1")
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

fn before_git_commit(command: &str) -> Option<&str> {
  let pattern = Regex::new(r"\bgit\s+(?:-C\s+\S+\s+)?commit\b").expect("valid git regex");
  pattern.find(command).map(|found| &command[..found.start()])
}

fn truncate(text: &str, limit: usize) -> String {
  text.chars().take(limit).collect()
}

fn without_heredoc_bodies(command: &str) -> String {
  let marker =
    Regex::new(r#"<<-?\s*['\"]?([A-Za-z_][A-Za-z0-9_]*)['\"]?"#).expect("valid heredoc regex");
  let mut delimiter = None;
  let mut kept = Vec::new();
  for line in command.lines() {
    if let Some(expected) = delimiter.as_deref() {
      if line.trim() == expected {
        delimiter = None;
      }
      continue;
    }
    kept.push(line);
    if let Some(capture) = marker.captures(line) {
      delimiter = Some(capture[1].to_owned());
    }
  }
  kept.join("\n").replace("\\\n", " ")
}

fn shell_parts(command: &str) -> Vec<String> {
  let pattern = Regex::new(r"(?:\n|&&|\|\||;|\|)+").expect("valid shell regex");
  pattern
    .split(without_heredoc_bodies(command).trim())
    .map(str::trim)
    .filter(|part| !part.is_empty())
    .map(str::to_owned)
    .collect()
}

fn command_harmless_after_gate(command: &str, run_dir: &Path) -> bool {
  let assignment = Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)=(.*)$").expect("valid assignment regex");
  let mut variables = HashMap::new();
  let mut cwd = run_dir.to_path_buf();
  for part in shell_parts(command) {
    let mut tokens = part.split_whitespace();
    for token in tokens.by_ref() {
      let Some(capture) = assignment.captures(token) else {
        if token == "cd" {
          let Some(target) = tokens
            .next()
            .and_then(|target| expand_path(target, &variables, &cwd))
          else {
            return false;
          };
          cwd = target;
        }
        break;
      };
      variables.insert(capture[1].to_owned(), unquote(&capture[2]).to_owned());
    }
    if harmless_after_gate(&part, run_dir, &variables, &cwd) {
      continue;
    }
    if !cwd.starts_with(run_dir) && !command.contains(&run_dir.to_string_lossy().into_owned()) {
      continue;
    }
    return false;
  }
  true
}

fn harmless_after_gate(
  part: &str,
  run_dir: &Path,
  variables: &HashMap<String, String>,
  cwd: &Path,
) -> bool {
  let redirects = Regex::new(r#"(?:\d+\s*)?(>&|<&|>>|<<|>|<)\s*("[^"]*"|'[^']*'|[^\s;|&]+)"#)
    .expect("valid redirect regex");
  for capture in redirects.captures_iter(part) {
    let operator = &capture[1];
    let target = unquote(&capture[2]);
    if operator.starts_with('<') && operator != "<>" {
      continue;
    }
    if operator.contains('&') && (target.chars().all(|c| c.is_ascii_digit()) || target == "-") {
      continue;
    }
    let Some(path) = expand_path(target, variables, cwd) else {
      return false;
    };
    if !path.starts_with(run_dir) || git_ignored(run_dir, &path) {
      continue;
    }
    return false;
  }
  let cleaned = command_backbone(part);
  let assignment = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*=").expect("valid assignment regex");
  let mut tokens: Vec<_> = cleaned.split_whitespace().collect();
  while tokens
    .first()
    .is_some_and(|token| assignment.is_match(token))
  {
    tokens.remove(0);
  }
  let Some(program) = tokens.first().copied() else {
    return true;
  };
  match program {
    "mkdir" => true,
    "git" => git_subcommand(&tokens).is_some_and(|command| {
      [
        "add",
        "status",
        "diff",
        "rev-parse",
        "log",
        "show",
        "ls-files",
        "cat-file",
        "ls-tree",
        "diff-tree",
        "rev-list",
        "name-rev",
        "symbolic-ref",
        "hash-object",
        "describe",
        "version",
        "help",
      ]
      .contains(&command)
    }),
    "find" => !tokens.iter().any(|token| {
      [
        "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fls", "-fprint", "-fprint0",
      ]
      .contains(token)
        || token.starts_with("-fprintf")
    }),
    "echo" | "printf" | "true" | ":" | "ls" | "pwd" | "cat" | "head" | "tail" | "wc" | "test"
    | "[" | "date" | "cd" => true,
    "rm" => safe_rm(&tokens),
    _ => false,
  }
}

fn unquote(token: &str) -> &str {
  token
    .strip_prefix('"')
    .and_then(|token| token.strip_suffix('"'))
    .or_else(|| {
      token
        .strip_prefix('\'')
        .and_then(|token| token.strip_suffix('\''))
    })
    .unwrap_or(token)
}

fn expand_path(token: &str, variables: &HashMap<String, String>, cwd: &Path) -> Option<PathBuf> {
  let variable = Regex::new(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?").expect("valid variable regex");
  let expanded = variable
    .replace_all(unquote(token), |capture: &regex::Captures<'_>| {
      variables
        .get(&capture[1])
        .cloned()
        .unwrap_or_else(|| capture[0].to_owned())
    })
    .into_owned();
  if expanded.contains('$') {
    return None;
  }
  let path = PathBuf::from(expanded);
  let path = if path.is_absolute() {
    path
  } else {
    cwd.join(path)
  };
  resolve_path(&path)
}

fn resolve_path(path: &Path) -> Option<PathBuf> {
  let ancestor = path.ancestors().find(|ancestor| ancestor.exists())?;
  let mut resolved = ancestor.canonicalize().ok()?;
  resolved.push(path.strip_prefix(ancestor).ok()?);
  Some(normalize_path(resolved))
}

fn normalize_path(path: PathBuf) -> PathBuf {
  use std::path::Component;

  let mut normalized = PathBuf::new();
  for component in path.components() {
    match component {
      Component::CurDir => {}
      Component::ParentDir => {
        normalized.pop();
      }
      other => normalized.push(other.as_os_str()),
    }
  }
  normalized
}

fn git_ignored(run_dir: &Path, path: &Path) -> bool {
  let Ok(relative) = path.strip_prefix(run_dir) else {
    return false;
  };
  ProcessCommand::new("git")
    .arg("-C")
    .arg(run_dir)
    .args(["check-ignore", "-q", "--"])
    .arg(relative)
    .output()
    .is_ok_and(|output| output.status.success())
}

fn git_subcommand<'a>(tokens: &'a [&str]) -> Option<&'a str> {
  let mut index = 1;
  while index < tokens.len() {
    match tokens[index] {
      "-C" | "--git-dir" | "--work-tree" | "-c" => index += 2,
      token if token.starts_with('-') => index += 1,
      token => return Some(token),
    }
  }
  None
}

fn safe_rm(tokens: &[&str]) -> bool {
  let mut paths = Vec::new();
  let mut options_ended = false;
  for token in &tokens[1..] {
    if !options_ended && *token == "--" {
      options_ended = true;
    } else if options_ended || !token.starts_with('-') {
      paths.push(*token);
    }
  }
  !paths.is_empty() && paths.into_iter().all(looks_gitignored)
}

fn looks_gitignored(path: &str) -> bool {
  const DIRS: &[&str] = &[
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".coverage",
    ".eggs",
    ".cache",
  ];
  let trimmed = path.trim_end_matches('/');
  let name = Path::new(trimmed)
    .file_name()
    .and_then(|name| name.to_str())
    .unwrap_or_default();
  DIRS.contains(&name)
    || [".pyc", ".pyo", ".pyd", ".egg-info"]
      .iter()
      .any(|suffix| name.ends_with(suffix))
    || name.ends_with('~')
    || DIRS
      .iter()
      .any(|dir| format!("/{trimmed}/").contains(&format!("/{dir}/")))
}

fn cmd_accept(store: &Store, task_id: i64, reason: &str) -> Result<()> {
  let Some(task) = store.task(task_id)? else {
    bail!("supervisor: no task {task_id}");
  };
  if reason.trim().is_empty() {
    bail!("supervisor: accept requires a non-empty --reason");
  }
  let mut statement = store
    .db
    .prepare("select state from task_events where task_id=?")?;
  let states = statement
    .query_map([task_id], |row| row.get::<_, String>(0))?
    .collect::<rusqlite::Result<HashSet<_>>>()?;
  if states.contains("verified") {
    bail!("supervisor: task {task_id} is already verified");
  }
  if states.contains("accepted") {
    bail!("supervisor: task {task_id} is already accepted");
  }
  if !matches!(task.state.as_str(), "committed" | "ingested") || task.commit_sha.is_none() {
    bail!(
      "supervisor: task {task_id} is {}, not a committed unverified task",
      task.state
    );
  }
  store.db.execute(
    "update tasks set reason=? where id=?",
    params![reason, task_id],
  )?;
  if task.state == "committed" {
    store.set_task_state(task_id, TaskState::Accepted)?;
  } else {
    let transaction = store.db.unchecked_transaction()?;
    task_event::create(&transaction, task_id, TaskState::Accepted)?;
    transaction.commit()?;
  }
  store.event("accepted", &format!("task {task_id}: {reason}"))?;
  println!("task {task_id} accepted without verify: {reason}");
  Ok(())
}

fn failures_in_lineage(store: &Store, task_id: i64) -> Result<i64> {
  let mut failures = 0;
  let mut current = store.task(task_id)?;
  while let Some(task) = current {
    if task.state == "failed" {
      failures += 1;
    }
    current = match task.retry_of_task_id {
      Some(retry_of_task_id) => store.task(retry_of_task_id)?,
      None => None,
    };
  }
  Ok(failures)
}

fn cmd_fail(store: &Store, task_id: i64, reason: &str) -> Result<()> {
  let task = store.task(task_id)?;
  if !task
    .as_ref()
    .is_some_and(|task| matches!(task.state.as_str(), "dispatched" | "in_flight"))
  {
    bail!("supervisor: task {task_id} is not in flight");
  }
  let dirty = git_stdout(store, &["status", "--porcelain"])?;
  store.db.execute(
    "update tasks set reason=? where id=?",
    params![reason, task_id],
  )?;
  store.set_task_state(task_id, TaskState::Failed)?;
  store.event("failed", &format!("task {task_id}: {reason}"))?;
  let failures = failures_in_lineage(store, task_id)?;
  let plural = if failures == 1 { "" } else { "s" };
  println!("task {task_id} failed ({failures} failure{plural} on this task): {reason}");
  if !dirty.is_empty() {
    println!("WARNING: tree is dirty — the implementer did not leave it clean:\n{dirty}");
  }
  if failures >= 3 {
    println!("three failures on the same task: escalate to the human");
  } else {
    println!(
      "adjust the task and retry with a fresh implementer: task new --retry-of {task_id} < task.md"
    );
  }
  Ok(())
}

fn cmd_calibrate(store: &Store, task_id: i64) -> Result<()> {
  let Some(task) = store.task(task_id)? else {
    bail!("supervisor: task {task_id} has no commit yet");
  };
  let Some(commit_sha) = task.commit_sha.as_deref() else {
    bail!("supervisor: task {task_id} has no commit yet");
  };
  let stat = git_stdout(store, &["show", "--shortstat", "--format=", commit_sha])?;
  let actual_files = stat_number(&stat, "file");
  let actual_lines = stat_number(&stat, "insertion") + stat_number(&stat, "deletion");
  let dispatched_at: Option<f64> = store
    .db
    .query_row(
      "select created_at / 1000.0 from task_events where task_id=? and state='dispatched'",
      [task_id],
      |row| row.get(0),
    )
    .optional()?;
  let committed_at: Option<f64> = store
    .db
    .query_row(
      "select created_at / 1000.0 from task_events where task_id=? and state='committed'",
      [task_id],
      |row| row.get(0),
    )
    .optional()?;
  let wall = dispatched_at
    .zip(committed_at)
    .map(|(start, end)| end - start);
  let log = match task.session_id {
    Some(session_id) => match store.session(session_id)? {
      Some(session) => session_log(store, &session)?,
      None => None,
    },
    None => None,
  };
  let next_offset = store
    .tasks()?
    .into_iter()
    .find(|candidate| {
      candidate.id > task_id && candidate.session_id == task.session_id && candidate.log_offset > 0
    })
    .map(|candidate| candidate.log_offset as u64);
  let mut end = context_peak(log.as_deref(), task.log_offset as u64, next_offset) as i64;
  if end == 0
    && let Some(session_id) = task.session_id
  {
    end = store
      .session(session_id)?
      .map_or(0, |session| session.context_max);
  }
  let base = task.context_size_start.unwrap_or_default();
  let context = (end - base).max(0);
  let transaction = store.db.unchecked_transaction()?;
  calibration::create(
    &transaction,
    task_id,
    task.predicted_files,
    task.predicted_lines,
    actual_files,
    actual_lines,
    wall,
    base,
    end,
  )?;
  transaction.commit()?;
  let wall_text = wall.map_or_else(|| "None".to_owned(), |wall| (wall as i64).to_string());
  let reuse = if task.is_session_reuse { ", reuse" } else { "" };
  println!(
    "task {task_id}: predicted {} files/{} lines, actual {actual_files} files/{actual_lines} lines, wall {wall_text}s, context {context} (session {end}, base {base}{reuse})",
    task.predicted_files, task.predicted_lines
  );
  Ok(())
}

fn cmd_disposition(
  store: &Store,
  finding: &str,
  verdict: &Verdict,
  task_id: Option<i64>,
  reason: &str,
) -> Result<()> {
  store.db.execute(
    "insert into dispositions(finding,verdict,task_id,reason,at) values(?,?,?,?,?)",
    params![finding, verdict.as_str(), task_id, reason, now()],
  )?;
  write_dispositions_view(store)
}

fn write_dispositions_view(store: &Store) -> Result<()> {
  let mut lines = vec![
    "# Dispositions (generated by the supervisor; do not edit)".to_owned(),
    String::new(),
  ];
  let mut statement = store
    .db
    .prepare("select finding,verdict,task_id,reason,at from dispositions order by id")?;
  let rows = statement.query_map([], |row| {
    Ok((
      row.get::<_, String>(0)?,
      row.get::<_, String>(1)?,
      row.get::<_, Option<i64>>(2)?,
      row.get::<_, String>(3)?,
      row.get::<_, f64>(4)?,
    ))
  })?;
  for row in rows {
    let (finding, verdict, task_id, reason, timestamp) = row?;
    let when = Local
      .timestamp_opt(timestamp as i64, 0)
      .single()
      .map_or_else(
        || "-".to_owned(),
        |time| time.format("%Y-%m-%d %H:%M").to_string(),
      );
    let target = task_id.map_or_else(String::new, |id| format!(" (task {id})"));
    lines.push(format!(
      "- {when} · {verdict}{target} · {finding} — {reason}"
    ));
  }
  fs::write(
    store.logs_dir.join("chainsaw-dispositions.md"),
    format!("{}\n", lines.join("\n")),
  )?;
  Ok(())
}

fn cmd_comments(store: &Store, show_all: bool) -> Result<()> {
  let path = store.logs_dir.join("chainsaw-comments.md");
  let Ok(text) = fs::read_to_string(path) else {
    println!("no comments file yet");
    return Ok(());
  };
  let offset = if show_all {
    0
  } else {
    store
      .cfg_or("comments-read", "0")?
      .parse::<usize>()
      .unwrap_or_default()
  };
  let new = text.get(offset..).unwrap_or_default();
  if new.trim().is_empty() {
    println!("no new comments");
  } else {
    println!("{new}");
  }
  store.set_cfg("comments-read", &text.len().to_string())?;
  Ok(())
}

fn cmd_state(store: &Store) -> Result<()> {
  println!("tasks");
  for task in store.tasks()? {
    let mut statement = store
      .db
      .prepare("select state,created_at / 1000.0 from task_events where task_id=?")?;
    let log = statement
      .query_map([task.id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
      })?
      .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    let timeline = TASK_STATES
      .iter()
      .filter_map(|state| {
        log
          .get(*state)
          .map(|timestamp| format!("{state}@{}", clock_time(*timestamp)))
      })
      .collect::<Vec<_>>()
      .join(" ");
    let retry = task
      .retry_of_task_id
      .map_or_else(String::new, |id| format!("  retry of {id}"));
    let reuse = if task.is_session_reuse {
      format!(
        "  reuse (context base {})",
        task.context_size_start.unwrap_or_default()
      )
    } else {
      String::new()
    };
    let reason = task
      .reason
      .as_deref()
      .map_or_else(String::new, |reason| format!("  reason: {reason}"));
    let session_name = match task.session_id {
      Some(session_id) => store
        .session(session_id)?
        .map_or_else(|| "-".to_owned(), |session| session.name),
      None => "-".to_owned(),
    };
    println!(
      "  {:>3} {:<10} {:<16} {:<10} {timeline}{retry}{reuse}{reason}",
      task.id,
      task.state,
      session_name,
      task.commit_sha.as_deref().map(short_sha).unwrap_or("-")
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
    if session_log(store, &session)?.is_some() {
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
  let unread = file_size(Some(&store.logs_dir.join("chainsaw-comments.md"))) as i64
    - store
      .cfg_or("comments-read", "0")?
      .parse::<i64>()
      .unwrap_or_default();
  if unread > 0 {
    println!("comments  {unread} bytes unread (chainsaw comments)");
  }
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
  for task in store.tasks()? {
    let start: Option<f64> = store
      .db
      .query_row(
        "select created_at / 1000.0 from task_events where task_id=? and state='dispatched'",
        [task.id],
        |row| row.get(0),
      )
      .optional()?;
    let end: Option<f64> = store
            .db
            .query_row(
                "select created_at / 1000.0 from task_events where task_id=? and state in ('committed','verified','ingested','failed') order by created_at limit 1",
                [task.id],
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

fn cmd_context(store: &Store, name: Option<&str>) -> Result<()> {
  for session in store
    .sessions()?
    .into_iter()
    .filter(|session| name.is_none_or(|name| session.name == name))
  {
    let log = session_log(store, &session)?;
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

fn daemon_prompt(store: &Store, name: &str, text: &str) -> bool {
  match cmd_prompt(store, name, text, false, 300) {
    Ok(()) => true,
    Err(error) => {
      let _ = store.event("prompt-unreachable", &format!("{name}: {error}"));
      false
    }
  }
}

fn daemon(store: &Store, lead: &str) -> Result<()> {
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
      let log = session_log(store, &session)?;
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
        "implementer" => observe_implementer(store, &session, Some(&log), quiet)?,
        "commentator" => {
          observe_commentator(store, &session, Some(&log), context, quiet, &mut compacting)?;
        }
        "lead" => observe_lead(store, name, context)?,
        _ => {}
      }
    }
    thread::sleep(Duration::from_secs(POLL_SECONDS));
  }
  store.event("daemon-exit", "")
}

fn observe_implementer(
  store: &Store,
  session: &Session,
  log: Option<&Path>,
  quiet: f64,
) -> Result<()> {
  let task = store.tasks()?.into_iter().rev().find(|task| {
    task.session_id == Some(session.id) && matches!(task.state.as_str(), "dispatched" | "in_flight")
  });
  let Some(task) = task else {
    return Ok(());
  };
  let shas = log
    .map(|path| commits_in_log(path, task.log_offset as u64))
    .unwrap_or_default();
  if let Some(sha) = new_commit_for(store, &shas, task.base_head.as_deref())? {
    store.db.execute(
      "update tasks set commit_sha=? where id=?",
      params![sha, task.id],
    )?;
    store.set_task_state(task.id, TaskState::Committed)?;
    store.event("committed", &format!("task {} {sha}", task.id))?;
  } else if quiet > STALE_SECONDS
    && session.kicked_at.is_none()
    && herdr_status(&session.name)
      .as_deref()
      .is_some_and(|status| matches!(status, "idle" | "done"))
  {
    store.event("kick", &session.name)?;
    store.db.execute(
      "update sessions set kicked_at=? where id=?",
      params![now(), session.id],
    )?;
    daemon_prompt(store, &session.name, "continue");
  }
  Ok(())
}

fn observe_commentator(
  store: &Store,
  session: &Session,
  log: Option<&Path>,
  context: i64,
  quiet: f64,
  compacting: &mut bool,
) -> Result<()> {
  let pending = store.tasks()?.into_iter().filter(|task| {
    matches!(task.state.as_str(), "verified" | "committed" | "accepted")
      && task.commit_sha.is_some()
  });
  if let Some(log) = log {
    let text = fs::read_to_string(log).unwrap_or_default();
    for task in pending {
      let sha = task.commit_sha.as_deref().unwrap_or_default();
      let abbreviation = sha.get(..7).unwrap_or(sha);
      if text.contains(abbreviation) {
        store.set_task_state(task.id, TaskState::Ingested)?;
        store.event("ingested", &format!("task {}", task.id))?;
      }
    }
  }
  if context > COMMENTATOR_COMPACT_TOKENS && !*compacting {
    *compacting = true;
    store.event("compact", &format!("{} at {context}", session.name))?;
    daemon_prompt(store, &session.name, "/compact");
  } else if context < COMMENTATOR_COMPACT_TOKENS {
    *compacting = false;
  }
  if quiet > STALE_SECONDS
    && session.kicked_at.is_none()
    && herdr_status(&session.name)
      .as_deref()
      .is_some_and(|status| matches!(status, "idle" | "done"))
  {
    store.event("kick", &session.name)?;
    store.db.execute(
      "update sessions set kicked_at=? where id=?",
      params![now(), session.id],
    )?;
    daemon_prompt(store, &session.name, "continue");
  }
  Ok(())
}

fn observe_lead(store: &Store, name: &str, context: i64) -> Result<()> {
  if context > LEAD_STOP_TOKENS && store.cfg("lead-told-stop")?.as_deref() != Some("1") {
    store.set_cfg("lead-told-stop", "1")?;
    store.event("stop-lead", &format!("context {context}"))?;
    daemon_prompt(
      store,
      name,
      &format!(
        "supervisor: your context is {context} tokens, past {LEAD_STOP_TOKENS}. Stop the run per the skill's Stopping section: the human must ask them plainly whether to end the run for good, then let the in-flight implementer finish, wait for the commentator on that commit, write the continuation prompt, then stop."
      ),
    );
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::{gate_last_problem, shell_parts};
  use std::path::Path;

  #[test]
  fn splits_compound_shell_commands() {
    assert_eq!(
      shell_parts("cargo test && git status"),
      ["cargo test", "git status"]
    );
  }

  #[test]
  fn gate_must_precede_commit() {
    let commands = vec![("git commit -m test".to_owned(), true)];
    assert!(
      gate_last_problem(&commands, "cargo test", Path::new("/tmp/run"))
        .unwrap()
        .contains("did not run before")
    );
  }
}
