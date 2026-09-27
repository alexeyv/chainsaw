//! Orca: sessions are terminals Orca knows by handle, each in a tab or pane it
//! opened. Orca has no session names, so the supervisor keeps its own map from
//! name to handle beside the run's database. Orca does not report the agent's
//! session id either, so the supervisor assigns one when the agent takes it on
//! the command line and otherwise reads it from the transcript the agent
//! starts writing.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{SessionQuery, SessionRuntime, StartSession, StartedSession};
use crate::domain::SessionKind;
use crate::infra::agent::{self, Agent, Claude};

/// The registry's name, beside the run's database.
pub const REGISTRY_FILE_NAME: &str = "chainsaw-orca-terminals.json";

/// Drives sessions through the `orca` CLI.
pub struct OrcaSessionRuntime {
  program: OsString,
  /// The terminal the supervisor itself runs in; the commentator's pane is
  /// split from it.
  terminal: String,
  /// Where session names are mapped to the terminals Orca opened for them.
  registry: PathBuf,
  /// How long a probe gives a session to prove itself idle.
  idle_probe: Duration,
  /// How long `wait` keeps probing a session that looks idle before taking
  /// its turn to be over: Orca notices a turn a moment after the prompt lands.
  turn_start_grace: Duration,
  /// How long `wait` lets a finished turn settle: the agent writes the turn's
  /// last transcript entry a moment after its TUI is back at the prompt.
  turn_end_settle: Duration,
  /// How long, and how many times, `start` polls for the session id of an
  /// agent that names its own sessions.
  session_id_poll_interval: Duration,
  session_id_poll_attempts: usize,
}

/// One session's terminal, as the registry remembers it.
#[derive(Debug, Serialize, Deserialize)]
struct Terminal {
  handle: String,
  tab_id: String,
  external_id: String,
}

/// What a terminal is doing, as far as a probe can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Activity {
  /// The TUI is at its prompt.
  Idle,
  /// The TUI is mid-turn.
  Busy,
  /// Orca no longer has the terminal.
  Gone,
}

/// What Orca answers when it does not do as asked. A probe tells a session
/// that is busy from one that is gone by the code.
#[derive(Debug)]
struct Refusal {
  code: String,
  message: String,
}

impl fmt::Display for Refusal {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "orca refused: {} ({})", self.message, self.code)
  }
}

impl std::error::Error for Refusal {}

impl OrcaSessionRuntime {
  /// Inside `terminal`, for sessions in `run_dir`.
  pub fn from_environment(run_dir: &Path, terminal: String) -> Result<Self> {
    let run_dir = run_dir
      .canonicalize()
      .with_context(|| format!("cannot resolve run directory {}", run_dir.display()))?;
    Ok(Self {
      program: OsString::from("orca"),
      terminal,
      registry: Claude::transcripts_dir(&run_dir)?.join(REGISTRY_FILE_NAME),
      idle_probe: Duration::from_millis(250),
      turn_start_grace: Duration::from_secs(5),
      turn_end_settle: Duration::from_secs(1),
      session_id_poll_interval: Duration::from_secs(2),
      session_id_poll_attempts: 30,
    })
  }

  fn run(&self, args: &[&str]) -> Result<Output> {
    Command::new(&self.program)
      .args(args)
      .output()
      .with_context(|| format!("failed to run {}", self.program.to_string_lossy()))
  }

  /// Orca's `result`, or its refusal. Orca reports a refusal in its JSON
  /// reply whatever its exit status says.
  fn request(&self, args: &[&str]) -> Result<Value> {
    let output = self.run(args)?;
    let reply: Value = match serde_json::from_slice(&output.stdout) {
      Ok(reply) => reply,
      Err(_) if !output.status.success() => bail!(
        "orca failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      ),
      Err(error) => return Err(error).context("orca returned invalid JSON"),
    };
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
      return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
    }
    Err(
      Refusal {
        code: json_string(&reply, "/error/code").unwrap_or_else(|_| "unknown".to_owned()),
        message: json_string(&reply, "/error/message").unwrap_or_default(),
      }
      .into(),
    )
  }

  fn registry(&self) -> Result<BTreeMap<String, Terminal>> {
    match fs::read(&self.registry) {
      Ok(bytes) => serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a terminal registry", self.registry.display())),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
      Err(error) => Err(error).with_context(|| format!("cannot read {}", self.registry.display())),
    }
  }

  fn remember(&self, name: &str, terminal: Terminal) -> Result<()> {
    let mut registry = self.registry()?;
    registry.insert(name.to_owned(), terminal);
    if let Some(parent) = self.registry.parent() {
      fs::create_dir_all(parent)?;
    }
    let temporary = self.registry.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&registry)?)?;
    fs::rename(&temporary, &self.registry)?;
    Ok(())
  }

  fn terminal_of(&self, name: &str) -> Result<Terminal> {
    self
      .registry()?
      .remove(name)
      .with_context(|| format!("orca: no session named {name}"))
  }

  /// Idle when the terminal's TUI settles within the probe, busy when it does
  /// not, gone when Orca no longer has the terminal.
  fn activity(&self, handle: &str) -> Activity {
    let probe = self.idle_probe.as_millis().to_string();
    let waited = self.request(&[
      "terminal",
      "wait",
      "--terminal",
      handle,
      "--for",
      "tui-idle",
      "--timeout-ms",
      &probe,
      "--json",
    ]);
    match waited {
      Ok(result) if result.pointer("/wait/satisfied").and_then(Value::as_bool) == Some(false) => {
        Activity::Busy
      }
      Ok(_) => Activity::Idle,
      Err(error)
        if error
          .downcast_ref::<Refusal>()
          .is_some_and(|refusal| refusal.code == "timeout") =>
      {
        Activity::Busy
      }
      Err(_) => Activity::Gone,
    }
  }

  /// The id an agent that names its own sessions gave the one it started in
  /// `run_dir` at `since`, once its transcript appears.
  fn session_id_written_by(
    &self,
    agent: &dyn Agent,
    run_dir: &Path,
    since: SystemTime,
  ) -> Option<String> {
    for attempt in 0..self.session_id_poll_attempts {
      if attempt > 0 {
        thread::sleep(self.session_id_poll_interval);
      }
      if let Some(id) = agent.session_started_since(run_dir, since) {
        return Some(id);
      }
    }
    None
  }
}

impl SessionRuntime for OrcaSessionRuntime {
  fn start(&self, session: StartSession<'_>) -> Result<StartedSession> {
    let agent = agent::implementing(session.agent);
    let minted = mint_session_id()?;
    let assigned = agent.session_id_args(&minted);
    let mut words = vec![agent.program().to_owned()];
    words.extend(assigned.iter().flatten().cloned());
    words.extend(session.args.iter().cloned());
    let run_dir = session.run_dir.to_string_lossy();
    let command = format!(
      "cd {} && exec {}",
      shell_words::quote(&run_dir),
      shell_words::join(&words)
    );
    let since = SystemTime::now();
    let (opened, key) = match session.kind {
      SessionKind::Implementer => {
        let worktree = format!("path:{run_dir}");
        let opened = self.request(&[
          "terminal",
          "create",
          "--worktree",
          &worktree,
          "--title",
          session.id,
          "--command",
          &command,
          "--json",
        ])?;
        (opened, "/terminal")
      }
      SessionKind::Commentator => {
        let opened = self.request(&[
          "terminal",
          "split",
          "--terminal",
          &self.terminal,
          "--direction",
          "horizontal",
          "--command",
          &command,
          "--json",
        ])?;
        (opened, "/split")
      }
    };
    let handle = json_string(&opened, &format!("{key}/handle"))?;
    let tab_id = json_string(&opened, &format!("{key}/tabId"))?;
    let external_id = match assigned {
      Some(_) => minted,
      None => self
        .session_id_written_by(agent, session.run_dir, since)
        .with_context(|| {
          format!(
            "orca: {} in {handle} never wrote a transcript for {}",
            session.agent,
            session.run_dir.display()
          )
        })?,
    };
    self.remember(
      session.id,
      Terminal {
        handle: handle.clone(),
        tab_id: tab_id.clone(),
        external_id: external_id.clone(),
      },
    )?;
    Ok(StartedSession {
      external_id,
      pane_id: handle,
      tab_id,
    })
  }

  fn query(&self, session_id: &str) -> Result<Option<SessionQuery>> {
    let Ok(terminal) = self.terminal_of(session_id) else {
      return Ok(None);
    };
    let status = match self.activity(&terminal.handle) {
      Activity::Idle => "idle",
      Activity::Busy => "busy",
      Activity::Gone => return Ok(None),
    };
    Ok(Some(SessionQuery {
      external_id: terminal.external_id,
      status: status.to_owned(),
    }))
  }

  fn prompt(&self, session_id: &str, text: &str) -> Result<()> {
    let terminal = self.terminal_of(session_id)?;
    let _ = self.request(&[
      "terminal",
      "send",
      "--terminal",
      &terminal.handle,
      "--text",
      text,
      "--enter",
      "--json",
    ])?;
    Ok(())
  }

  fn interrupt(&self, session_id: &str) -> Result<()> {
    let terminal = self.terminal_of(session_id)?;
    let _ = self.request(&[
      "terminal",
      "send",
      "--terminal",
      &terminal.handle,
      "--interrupt",
      "--json",
    ])?;
    Ok(())
  }

  /// Until the session's turn is over and written down. A session that looks
  /// idle is probed again through the grace period, since Orca notices a turn
  /// a moment after the prompt lands; one still idle after that has nothing
  /// to wait for, and neither has one that is gone. Orca reports the turn
  /// over a moment before the agent records its last word, hence the settle.
  fn wait(&self, session_id: &str, timeout: Duration) -> Result<()> {
    let terminal = self.terminal_of(session_id)?;
    let grace_over = Instant::now() + self.turn_start_grace;
    loop {
      match self.activity(&terminal.handle) {
        Activity::Busy => break,
        Activity::Idle if Instant::now() < grace_over => thread::sleep(self.idle_probe),
        Activity::Idle | Activity::Gone => return Ok(()),
      }
    }
    let timeout_ms = timeout.as_millis().to_string();
    let _ = self.run(&[
      "terminal",
      "wait",
      "--terminal",
      &terminal.handle,
      "--for",
      "tui-idle",
      "--timeout-ms",
      &timeout_ms,
      "--json",
    ])?;
    thread::sleep(self.turn_end_settle);
    Ok(())
  }
}

fn json_string(value: &Value, pointer: &str) -> Result<String> {
  value
    .pointer(pointer)
    .and_then(Value::as_str)
    .map(str::to_owned)
    .with_context(|| format!("orca response lacks {pointer}"))
}

/// A fresh version 4 UUID, for an agent that takes its session id on the
/// command line.
fn mint_session_id() -> Result<String> {
  let mut bytes = [0u8; 16];
  fs::File::open("/dev/urandom")
    .and_then(|mut random| random.read_exact(&mut bytes))
    .context("cannot read /dev/urandom")?;
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
  Ok(format!(
    "{}-{}-{}-{}-{}",
    &hex[..8],
    &hex[8..12],
    &hex[12..16],
    &hex[16..20],
    &hex[20..]
  ))
}

#[cfg(test)]
mod tests;
