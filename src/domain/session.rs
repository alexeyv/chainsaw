use std::fmt;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use super::{require_nonblank, require_nonnegative, require_optional_nonblank, require_positive};

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

impl fmt::Display for Role {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(self.as_str())
  }
}

/// One Claude Code session under the supervisor's watch. A row is one
/// incarnation: relaunching the same name stops this one and starts another.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
  id: i64,
  name: String,
  role: Role,
  external_session_id: String,
  launched_head: Option<String>,
  started_at: DateTime<Utc>,
  stopped_at: Option<DateTime<Utc>>,
  context: i64,
  context_max: i64,
  last_growth: DateTime<Utc>,
  kicked_at: Option<DateTime<Utc>>,
}

impl Session {
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    id: i64,
    name: String,
    role: Role,
    external_session_id: String,
    launched_head: Option<String>,
    started_at: DateTime<Utc>,
    stopped_at: Option<DateTime<Utc>>,
    context: i64,
    context_max: i64,
    last_growth: DateTime<Utc>,
    kicked_at: Option<DateTime<Utc>>,
  ) -> Result<Self> {
    require_positive("id", id)?;
    require_nonblank("name", &name)?;
    require_nonblank("external_session_id", &external_session_id)?;
    require_optional_nonblank("launched_head", launched_head.as_deref())?;
    require_nonnegative("context", context)?;
    require_nonnegative("context_max", context_max)?;
    if context_max < context {
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

    Ok(Self {
      id,
      name,
      role,
      external_session_id,
      launched_head,
      started_at,
      stopped_at,
      context,
      context_max,
      last_growth,
      kicked_at,
    })
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

  pub fn context(&self) -> i64 {
    self.context
  }

  pub fn context_max(&self) -> i64 {
    self.context_max
  }

  pub fn last_growth(&self) -> DateTime<Utc> {
    self.last_growth
  }

  pub fn kicked_at(&self) -> Option<DateTime<Utc>> {
    self.kicked_at
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
}

#[cfg(test)]
mod tests;
