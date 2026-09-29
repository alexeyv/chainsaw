//! Herdr: sessions are agents Herdr knows by name, each in a pane it opened.

use std::env;
use std::ffi::OsString;
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::domain::{
  AgentKind, SessionKind, SessionRuntime, SessionStatus, StartSession, StartedSession,
};

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

  /// What Herdr calls an agent chainsaw knows.
  fn agent_kind(agent: AgentKind) -> &'static str {
    match agent {
      AgentKind::Claude => "claude",
      AgentKind::Codex => "codex",
      AgentKind::Cursor => "cursor",
    }
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
      "agent",
      "start",
      session.id,
      "--kind",
      Self::agent_kind(session.agent),
      "--pane",
      &pane_id,
      "--",
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

  fn status(&self, session_id: &str) -> Result<Option<SessionStatus>> {
    let response = match self.request(&["agent", "get", session_id]) {
      Ok(response) => response,
      Err(_) => return Ok(None),
    };
    Ok(Some(status_named(&Self::json_string(
      &response,
      "/result/agent/status",
    )?)))
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

/// Herdr's agent states, as `agent wait --until` lists them: idle, working,
/// blocked, done, unknown. A done agent has left its prompt for good, and
/// takes nothing more; a blocked one is mid-turn, waiting on a permission.
fn status_named(status: &str) -> SessionStatus {
  match status {
    "idle" | "done" => SessionStatus::Idle,
    "working" | "blocked" => SessionStatus::Busy,
    _ => SessionStatus::Unknown,
  }
}

#[cfg(test)]
mod tests;
