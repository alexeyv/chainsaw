use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use fs2::FileExt;
use serde_json::{Map, Value, json};

use super::store;

pub const RUNTIME_ENV: &str = "CHAINSAW_SESSION_RUNTIME";
pub const ZERO_COST_DUMMY_STATE_ENV: &str = "CHAINSAW_ZERO_COST_DUMMY_STATE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
  Implementer,
  Commentator,
}

impl SessionKind {
  pub fn label(self) -> &'static str {
    match self {
      Self::Implementer => "implementer",
      Self::Commentator => "commentator",
    }
  }
}

pub struct StartSession<'a> {
  pub id: &'a str,
  pub run_dir: &'a Path,
  pub kind: SessionKind,
  /// The Claude flags the session launches with, verbatim.
  pub args: &'a [String],
}

#[derive(Debug)]
pub struct StartedSession {
  pub external_id: String,
  pub pane_id: String,
  pub tab_id: String,
}

#[derive(Debug)]
pub struct SessionQuery {
  pub external_id: String,
  pub status: String,
}

pub trait SessionRuntime {
  fn start(&self, session: StartSession<'_>) -> Result<StartedSession>;
  fn query(&self, session_id: &str) -> Result<Option<SessionQuery>>;
  fn prompt(&self, session_id: &str, text: &str) -> Result<()>;
  fn interrupt(&self, session_id: &str) -> Result<()>;
  fn wait(&self, session_id: &str, timeout: Duration) -> Result<()>;
}

pub fn from_environment() -> Result<Box<dyn SessionRuntime>> {
  match env::var(RUNTIME_ENV).ok().as_deref() {
    None | Some("herdr") => Ok(Box::new(HerdrSessionRuntime::from_environment())),
    Some("zero-cost-dummy" | "dummy") => Ok(Box::new(ZeroCostDummy::from_environment()?)),
    Some(value) => bail!("unknown {RUNTIME_ENV} value {value:?}"),
  }
}

/// Drives sessions through the `herdr` CLI. The pane the supervisor itself runs in
/// is ambient, so it is read once here rather than rediscovered inside `start`.
pub struct HerdrSessionRuntime {
  program: OsString,
  workspace: Option<String>,
  tab_id: String,
  /// How long, and how many times, `start` polls `agent get` for a session id
  /// that `agent start` did not report.
  session_id_poll_interval: Duration,
  session_id_poll_attempts: usize,
}

impl HerdrSessionRuntime {
  pub fn from_environment() -> Self {
    Self {
      program: OsString::from("herdr"),
      workspace: env::var("HERDR_WORKSPACE_ID").ok(),
      tab_id: env::var("HERDR_TAB_ID").unwrap_or_default(),
      session_id_poll_interval: Duration::from_secs(2),
      session_id_poll_attempts: 30,
    }
  }

  fn run(&self, args: &[&str]) -> Result<Output> {
    Command::new(&self.program)
      .args(args)
      .output()
      .with_context(|| format!("failed to run {}", self.program.to_string_lossy()))
  }

  fn request(&self, args: &[&str]) -> Result<Value> {
    let output = self.run(args)?;
    if !output.status.success() {
      bail!(
        "herdr failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      );
    }
    serde_json::from_slice(&output.stdout).context("herdr returned invalid JSON")
  }

  fn json_string(value: &Value, pointer: &str) -> Result<String> {
    value
      .pointer(pointer)
      .and_then(Value::as_str)
      .map(str::to_owned)
      .with_context(|| format!("herdr response lacks {pointer}"))
  }
}

impl SessionRuntime for HerdrSessionRuntime {
  fn start(&self, session: StartSession<'_>) -> Result<StartedSession> {
    let workspace = self
      .workspace
      .as_deref()
      .ok_or_else(|| anyhow!("supervisor: must run inside a Herdr pane"))?;
    let run_dir = session.run_dir.to_string_lossy();
    let (pane_id, tab_id) = match session.kind {
      SessionKind::Commentator => {
        let response = self.request(&[
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
          Self::json_string(&response, "/result/pane/pane_id")?,
          self.tab_id.clone(),
        )
      }
      SessionKind::Implementer => {
        let response = self.request(&[
          "tab",
          "create",
          "--workspace",
          workspace,
          "--label",
          session.id,
          "--cwd",
          &run_dir,
          "--no-focus",
        ])?;
        (
          Self::json_string(&response, "/result/root_pane/pane_id")?,
          Self::json_string(&response, "/result/tab/tab_id")?,
        )
      }
    };

    let mut arguments = vec![
      "agent", "start", session.id, "--kind", "claude", "--pane", &pane_id, "--",
    ];
    arguments.extend(session.args.iter().map(String::as_str));
    let mut started = None;
    for attempt in 0..5 {
      match self.request(&arguments) {
        Ok(response) => {
          started = Some(response);
          break;
        }
        Err(error) if attempt == 4 => return Err(error),
        Err(_) => thread::sleep(Duration::from_secs(2)),
      }
    }
    let started = started.context("herdr agent did not start")?;
    // Under load `agent start` returns before the agent has reported its
    // session id; poll `agent get` until it appears rather than failing and
    // leaving an orphaned session that the supervisor never registered.
    let mut external_id = Self::json_string(&started, "/result/agent/agent_session/value");
    let mut attempt = 0;
    while external_id.is_err() && attempt < self.session_id_poll_attempts {
      thread::sleep(self.session_id_poll_interval);
      attempt += 1;
      if let Ok(response) = self.request(&["agent", "get", session.id]) {
        external_id = Self::json_string(&response, "/result/agent/agent_session/value");
      }
    }
    Ok(StartedSession {
      external_id: external_id.context("herdr agent never reported a session id")?,
      pane_id,
      tab_id,
    })
  }

  fn query(&self, session_id: &str) -> Result<Option<SessionQuery>> {
    let response = match self.request(&["agent", "get", session_id]) {
      Ok(response) => response,
      Err(_) => return Ok(None),
    };
    Ok(Some(SessionQuery {
      external_id: Self::json_string(&response, "/result/agent/agent_session/value")?,
      status: Self::json_string(&response, "/result/agent/status")?,
    }))
  }

  fn prompt(&self, session_id: &str, text: &str) -> Result<()> {
    let _ = self.run(&["agent", "prompt", session_id, text])?;
    Ok(())
  }

  fn interrupt(&self, session_id: &str) -> Result<()> {
    let _ = self.request(&["agent", "send-keys", session_id, "esc"])?;
    Ok(())
  }

  fn wait(&self, session_id: &str, timeout: Duration) -> Result<()> {
    let timeout_ms = timeout.as_millis().to_string();
    let _ = self.run(&["agent", "wait", session_id, "--timeout", &timeout_ms])?;
    Ok(())
  }
}

pub struct ZeroCostDummy {
  state_path: PathBuf,
}

impl ZeroCostDummy {
  pub fn new(state_path: PathBuf) -> Self {
    Self { state_path }
  }

  fn from_environment() -> Result<Self> {
    let state_path = env::var_os(ZERO_COST_DUMMY_STATE_ENV)
      .map(PathBuf::from)
      .with_context(|| format!("{ZERO_COST_DUMMY_STATE_ENV} is not set"))?;
    Ok(Self::new(state_path))
  }

  fn with_state<T>(&self, action: impl FnOnce(&mut Value) -> Result<T>) -> Result<T> {
    let lock_path = self.state_path.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
      fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
      .create(true)
      .write(true)
      .truncate(false)
      .open(lock_path)?;
    lock.lock_exclusive()?;
    let mut state = match fs::read(&self.state_path) {
      Ok(bytes) => serde_json::from_slice(&bytes).context("dummy runtime state is invalid JSON")?,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::empty_state(),
      Err(error) => return Err(error.into()),
    };
    let result = action(&mut state);
    if result.is_ok() {
      self.write_state(&state)?;
    }
    FileExt::unlock(&lock)?;
    result
  }

  fn empty_state() -> Value {
    json!({
      "agents": {},
      "panes": {},
      "sequence": 0,
      "drop_prompts": 0,
      "operations": [],
    })
  }

  fn write_state(&self, state: &Value) -> Result<()> {
    if let Some(parent) = self.state_path.parent() {
      fs::create_dir_all(parent)?;
    }
    let temporary = self.state_path.with_extension("tmp");
    let mut file = fs::File::create(&temporary)?;
    serde_json::to_writer(&mut file, state)?;
    file.flush()?;
    fs::rename(temporary, &self.state_path)?;
    Ok(())
  }

  fn object_mut<'a>(state: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>> {
    state
      .get_mut(key)
      .and_then(Value::as_object_mut)
      .with_context(|| format!("dummy runtime state field {key:?} is not an object"))
  }

  fn operations_mut(state: &mut Value) -> Result<&mut Vec<Value>> {
    state
      .get_mut("operations")
      .and_then(Value::as_array_mut)
      .context("dummy runtime state field \"operations\" is not an array")
  }

  fn logs_dir(run_dir: &Path) -> Result<PathBuf> {
    let run_dir = run_dir.canonicalize().with_context(|| {
      format!(
        "cannot resolve dummy session run directory {}",
        run_dir.display()
      )
    })?;
    store::logs_dir_for(&run_dir)
  }

  /// Write the transcript entries a real agent produces when it picks up a prompt.
  fn deliver(state: &Value, session: &Value, text: &str) -> Result<()> {
    let run_dir = session
      .get("run_dir")
      .and_then(Value::as_str)
      .context("dummy session lacks run_dir")?;
    let external_id = session
      .get("session_id")
      .and_then(Value::as_str)
      .context("dummy session lacks session_id")?;
    let log = Self::logs_dir(Path::new(run_dir))?.join(format!("{external_id}.jsonl"));
    Self::append_log(
      &log,
      &json!({
        "type": "user",
        "message": {"content": text},
      }),
    )?;
    if let Some(reply) = state.get("reply_on_prompt").and_then(Value::as_str) {
      Self::append_log(
        &log,
        &json!({
          "type": "assistant",
          "message": {"content": [{"type": "text", "text": reply}]},
        }),
      )?;
    }
    Ok(())
  }

  /// Write the transcript entry a real agent produces when it queues a prompt.
  fn enqueue(session: &Value, text: &str) -> Result<()> {
    let run_dir = session
      .get("run_dir")
      .and_then(Value::as_str)
      .context("dummy session lacks run_dir")?;
    let external_id = session
      .get("session_id")
      .and_then(Value::as_str)
      .context("dummy session lacks session_id")?;
    let log = Self::logs_dir(Path::new(run_dir))?.join(format!("{external_id}.jsonl"));
    Self::append_log(
      &log,
      &json!({
        "type": "queue-operation",
        "operation": "enqueue",
        "content": text,
        "timestamp": Utc::now().to_rfc3339(),
      }),
    )
  }

  fn is_busy(session: &Value) -> bool {
    session.get("status").and_then(Value::as_str) == Some("busy")
  }

  /// A real agent works through what it queued while busy as soon as it goes idle.
  fn drain_queue(state: &mut Value, session_id: &str) -> Result<()> {
    let Some(session) = Self::object_mut(state, "agents")?.get(session_id).cloned() else {
      return Ok(());
    };
    if Self::is_busy(&session) {
      return Ok(());
    }
    let queued: Vec<String> = session
      .get("queued")
      .and_then(Value::as_array)
      .map(|texts| {
        texts
          .iter()
          .filter_map(Value::as_str)
          .map(str::to_owned)
          .collect()
      })
      .unwrap_or_default();
    if queued.is_empty() {
      return Ok(());
    }
    for text in &queued {
      Self::deliver(state, &session, text)?;
    }
    if let Some(session) = Self::object_mut(state, "agents")?.get_mut(session_id) {
      session["queued"] = json!([]);
    }
    Ok(())
  }

  fn append_log(path: &Path, entry: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent)?;
    }
    let mut log = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut log, entry)?;
    writeln!(log)?;
    Ok(())
  }
}

impl SessionRuntime for ZeroCostDummy {
  fn start(&self, session: StartSession<'_>) -> Result<StartedSession> {
    self.with_state(|state| {
      let sequence = state
        .get("sequence")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        + 1;
      state["sequence"] = json!(sequence);
      let pane_id = format!("pane-{sequence}");
      let tab_id = format!("tab-{sequence}");
      let external_id = format!("session-{}-{sequence}", session.id);
      let run_dir = session.run_dir.to_string_lossy().into_owned();
      Self::object_mut(state, "panes")?.insert(pane_id.clone(), json!({"cwd": run_dir}));
      Self::object_mut(state, "agents")?.insert(
        session.id.to_owned(),
        json!({
          "session_id": external_id,
          "status": "idle",
          "run_dir": run_dir,
        }),
      );
      Self::operations_mut(state)?.push(json!({
        "operation": "start",
        "session_id": session.id,
        "kind": session.kind.label(),
        "args": session.args,
      }));
      Ok(StartedSession {
        external_id,
        pane_id,
        tab_id,
      })
    })
  }

  fn query(&self, session_id: &str) -> Result<Option<SessionQuery>> {
    self.with_state(|state| {
      Self::drain_queue(state, session_id)?;
      let session = Self::object_mut(state, "agents")?.get(session_id).cloned();
      Self::operations_mut(state)?.push(json!({
        "operation": "query",
        "session_id": session_id,
      }));
      session
        .map(|session| {
          Ok(SessionQuery {
            external_id: session
              .get("session_id")
              .and_then(Value::as_str)
              .context("dummy session lacks session_id")?
              .to_owned(),
            status: session
              .get("status")
              .and_then(Value::as_str)
              .unwrap_or("idle")
              .to_owned(),
          })
        })
        .transpose()
    })
  }

  fn prompt(&self, session_id: &str, text: &str) -> Result<()> {
    self.with_state(|state| {
      Self::operations_mut(state)?.push(json!({
        "operation": "prompt",
        "session_id": session_id,
        "text": text,
      }));
      let drop_prompts = state
        .get("drop_prompts")
        .and_then(Value::as_u64)
        .unwrap_or_default();
      if drop_prompts > 0 {
        state["drop_prompts"] = json!(drop_prompts - 1);
        return Ok(());
      }
      let session = Self::object_mut(state, "agents")?
        .get(session_id)
        .cloned()
        .with_context(|| format!("dummy session {session_id} does not exist"))?;
      if Self::is_busy(&session) {
        Self::enqueue(&session, text)?;
        let queued = Self::object_mut(state, "agents")?
          .get_mut(session_id)
          .and_then(|session| session.get_mut("queued"))
          .and_then(Value::as_array_mut);
        match queued {
          Some(queued) => queued.push(json!(text)),
          None => {
            if let Some(session) = Self::object_mut(state, "agents")?.get_mut(session_id) {
              session["queued"] = json!([text]);
            }
          }
        }
        return Ok(());
      }
      Self::drain_queue(state, session_id)?;
      Self::deliver(state, &session, text)
    })
  }

  fn interrupt(&self, session_id: &str) -> Result<()> {
    self.with_state(|state| {
      let session = Self::object_mut(state, "agents")?
        .get_mut(session_id)
        .with_context(|| format!("dummy session {session_id} does not exist"))?;
      session["status"] = json!("idle");
      session["queued"] = json!([]);
      Self::operations_mut(state)?.push(json!({
        "operation": "interrupt",
        "session_id": session_id,
      }));
      Ok(())
    })
  }

  fn wait(&self, session_id: &str, timeout: Duration) -> Result<()> {
    self.with_state(|state| {
      Self::operations_mut(state)?.push(json!({
        "operation": "wait",
        "session_id": session_id,
        "timeout_ms": timeout.as_millis(),
      }));
      Ok(())
    })?;
    let deadline = Instant::now() + timeout;
    loop {
      let idle = self.with_state(|state| {
        Self::drain_queue(state, session_id)?;
        Ok(
          Self::object_mut(state, "agents")?
            .get(session_id)
            .is_none_or(|session| !Self::is_busy(session)),
        )
      })?;
      if idle || Instant::now() >= deadline {
        return Ok(());
      }
      thread::sleep(Duration::from_millis(10));
    }
  }
}

#[cfg(test)]
mod tests;
