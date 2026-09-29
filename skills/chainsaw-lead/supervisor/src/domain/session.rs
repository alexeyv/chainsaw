use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use strum::EnumIter;

use super::{
  Agent, ContextSize, SessionRuntime, SessionStatus, require_nonblank, require_optional_nonblank,
  require_positive,
};

/// What a session is for. The lead runs the process, implementers take tasks,
/// and the commentator reviews commits; only implementers are ever dispatched to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
  Lead,
  Implementer,
  Commentator,
}

impl Role {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Lead => "lead",
      Self::Implementer => "implementer",
      Self::Commentator => "commentator",
    }
  }
}

impl TryFrom<&str> for Role {
  type Error = anyhow::Error;

  fn try_from(value: &str) -> Result<Self> {
    match value {
      "lead" => Ok(Self::Lead),
      "implementer" => Ok(Self::Implementer),
      "commentator" => Ok(Self::Commentator),
      value => bail!("unknown session role {value:?}"),
    }
  }
}

/// The kind of session the supervisor launches. The lead is never launched,
/// so it has no kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionKind {
  Implementer,
  Commentator,
}

impl SessionKind {
  pub fn label(self) -> &'static str {
    self.role().as_str()
  }

  /// The role a session of this kind is recorded with.
  pub fn role(self) -> Role {
    match self {
      Self::Implementer => Role::Implementer,
      Self::Commentator => Role::Commentator,
    }
  }
}

impl fmt::Display for Role {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(self.as_str())
  }
}

/// Which coding agent runs a session: Claude Code, OpenAI Codex or the Cursor
/// Agent CLI. The name is what the session row stores. Iterating the enum
/// lists every agent the supervisor accepts.
#[derive(Clone, Copy, Debug, EnumIter, Eq, PartialEq)]
pub enum AgentKind {
  Claude,
  Codex,
  Cursor,
}

impl AgentKind {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Claude => "claude",
      Self::Codex => "codex",
      Self::Cursor => "cursor",
    }
  }
}

impl TryFrom<&str> for AgentKind {
  type Error = anyhow::Error;

  fn try_from(value: &str) -> Result<Self> {
    match value {
      "claude" => Ok(Self::Claude),
      "codex" => Ok(Self::Codex),
      "cursor" => Ok(Self::Cursor),
      value => bail!("unknown agent {value:?}"),
    }
  }
}

impl fmt::Display for AgentKind {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(self.as_str())
  }
}

/// One agent session under the supervisor's watch. A row is one
/// incarnation: relaunching the same name stops this one and starts another.
/// It drives itself through the run's runtime, and reads its transcript
/// through the agent it was launched with; it borrows both from the run.
#[derive(Clone)]
pub struct Session<'r> {
  runtime: &'r dyn SessionRuntime,
  agent: &'r dyn Agent,
  id: i64,
  name: String,
  role: Role,
  agent_kind: AgentKind,
  external_session_id: String,
  launched_head: Option<String>,
  started_at: DateTime<Utc>,
  stopped_at: Option<DateTime<Utc>>,
  context: ContextSize,
  context_max: ContextSize,
  last_growth: DateTime<Utc>,
  kicked_at: Option<DateTime<Utc>>,
  over_limit_at: Option<DateTime<Utc>>,
  transcript: Option<PathBuf>,
}

impl<'r> Session<'r> {
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    runtime: &'r dyn SessionRuntime,
    agent: &'r dyn Agent,
    id: i64,
    name: String,
    role: Role,
    agent_kind: AgentKind,
    external_session_id: String,
    launched_head: Option<String>,
    started_at: DateTime<Utc>,
    stopped_at: Option<DateTime<Utc>>,
    context: ContextSize,
    context_max: ContextSize,
    last_growth: DateTime<Utc>,
    kicked_at: Option<DateTime<Utc>>,
    over_limit_at: Option<DateTime<Utc>>,
    transcript: Option<PathBuf>,
  ) -> Result<Self> {
    require_positive("id", id)?;
    require_nonblank("name", &name)?;
    require_nonblank("external_session_id", &external_session_id)?;
    require_optional_nonblank("launched_head", launched_head.as_deref())?;
    if transcript
      .as_deref()
      .is_some_and(|path| path.as_os_str().is_empty())
    {
      bail!("transcript cannot be blank");
    }
    if let (Some(context), Some(context_max)) = (context.known(), context_max.known())
      && context_max < context
    {
      bail!("context_max cannot be below context");
    }
    if stopped_at.is_some_and(|stopped| stopped < started_at) {
      bail!("stopped_at cannot precede started_at");
    }
    if last_growth < started_at {
      bail!("last_growth cannot precede started_at");
    }
    if kicked_at.is_some_and(|kicked| kicked < started_at) {
      bail!("kicked_at cannot precede started_at");
    }
    if over_limit_at.is_some_and(|over| over < started_at) {
      bail!("over_limit_at cannot precede started_at");
    }

    Ok(Self {
      runtime,
      agent,
      id,
      name,
      role,
      agent_kind,
      external_session_id,
      launched_head,
      started_at,
      stopped_at,
      context,
      context_max,
      last_growth,
      kicked_at,
      over_limit_at,
      transcript,
    })
  }

  /// What the runtime says the session is doing, or None when it has no such
  /// session or cannot be reached.
  pub fn status(&self) -> Option<SessionStatus> {
    self.runtime.status(&self.name).ok().flatten()
  }

  pub fn prompt(&self, text: &str) -> Result<()> {
    self.runtime.prompt(&self.name, text)
  }

  pub fn interrupt(&self) -> Result<()> {
    self.runtime.interrupt(&self.name)
  }

  /// Waits for the session's current turn to end, up to `timeout`.
  pub fn wait(&self, timeout: Duration) -> Result<()> {
    self.runtime.wait(&self.name, timeout)
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn name(&self) -> &str {
    &self.name
  }

  pub fn role(&self) -> Role {
    self.role
  }

  /// The kind of agent the session was launched with, as its row records.
  pub fn agent_kind(&self) -> AgentKind {
    self.agent_kind
  }

  /// The agent the session runs: the implementation of its kind.
  pub fn agent(&self) -> &'r dyn Agent {
    self.agent
  }

  pub fn external_session_id(&self) -> &str {
    &self.external_session_id
  }

  pub fn launched_head(&self) -> Option<&str> {
    self.launched_head.as_deref()
  }

  pub fn started_at(&self) -> DateTime<Utc> {
    self.started_at
  }

  pub fn stopped_at(&self) -> Option<DateTime<Utc>> {
    self.stopped_at
  }

  /// Context the session held at its latest reading: unknown while nothing
  /// has been read or the agent's transcript cannot say.
  pub fn context(&self) -> ContextSize {
    self.context
  }

  /// The largest context ever read for the session: unknown while no reading
  /// has said.
  pub fn context_max(&self) -> ContextSize {
    self.context_max
  }

  pub fn last_growth(&self) -> DateTime<Utc> {
    self.last_growth
  }

  pub fn kicked_at(&self) -> Option<DateTime<Utc>> {
    self.kicked_at
  }

  pub fn over_limit_at(&self) -> Option<DateTime<Utc>> {
    self.over_limit_at
  }

  /// Where the agent writes this session's transcript, once it has been
  /// found. It never moves.
  pub fn transcript(&self) -> Option<&Path> {
    self.transcript.as_deref()
  }

  /// A session is live until it is superseded or stopped.
  pub fn is_live(&self) -> bool {
    self.stopped_at.is_none()
  }

  /// Only a live implementer can be dispatched a task.
  pub fn can_take_task(&self) -> bool {
    self.role == Role::Implementer && self.is_live()
  }

  /// Whole seconds since the transcript last grew, as seen at `now`. Never
  /// negative, so a clock that runs behind reads as "just now".
  pub fn quiet_seconds(&self, now: DateTime<Utc>) -> i64 {
    (now - self.last_growth).num_seconds().max(0)
  }

  /// Whether a stalled session may be nudged: once per stall, and not again
  /// until the transcript has grown since the last nudge.
  pub fn can_be_kicked(&self) -> bool {
    self.is_live() && self.kicked_at.is_none()
  }

  /// Whether crossing the context stop threshold is still unrecorded for this
  /// incarnation. Latched once per session and never cleared, so a relaunched
  /// lead can cross it again.
  pub fn can_latch_over_limit(&self) -> bool {
    self.is_live() && self.over_limit_at.is_none()
  }
}

/// Two sessions are the same session when their records agree; the runtime
/// and the agent they borrow are the run's, not theirs.
impl PartialEq for Session<'_> {
  fn eq(&self, other: &Self) -> bool {
    self.id == other.id
      && self.name == other.name
      && self.role == other.role
      && self.agent_kind == other.agent_kind
      && self.external_session_id == other.external_session_id
      && self.launched_head == other.launched_head
      && self.started_at == other.started_at
      && self.stopped_at == other.stopped_at
      && self.context == other.context
      && self.context_max == other.context_max
      && self.last_growth == other.last_growth
      && self.kicked_at == other.kicked_at
      && self.over_limit_at == other.over_limit_at
      && self.transcript == other.transcript
  }
}

impl fmt::Debug for Session<'_> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter
      .debug_struct("Session")
      .field("id", &self.id)
      .field("name", &self.name)
      .field("role", &self.role)
      .field("agent_kind", &self.agent_kind)
      .field("external_session_id", &self.external_session_id)
      .field("launched_head", &self.launched_head)
      .field("started_at", &self.started_at)
      .field("stopped_at", &self.stopped_at)
      .field("context", &self.context)
      .field("context_max", &self.context_max)
      .field("last_growth", &self.last_growth)
      .field("kicked_at", &self.kicked_at)
      .field("over_limit_at", &self.over_limit_at)
      .field("transcript", &self.transcript)
      .finish()
  }
}

#[cfg(test)]
mod tests;
