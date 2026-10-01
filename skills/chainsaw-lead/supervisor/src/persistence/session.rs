use std::path::Path;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::domain::{Agent, AgentKind, ContextSize, Role, Session, SessionRuntime};

struct SessionRow {
  id: i64,
  name: String,
  role: String,
  agent: String,
  external_session_id: String,
  launched_head: Option<String>,
  started_at: i64,
  stopped_at: Option<i64>,
  context: Option<i64>,
  context_max: Option<i64>,
  last_growth: i64,
  kicked_at: Option<i64>,
  over_limit_at: Option<i64>,
  transcript: Option<String>,
}

const SELECT: &str = "
  select id, name, role, agent, external_session_id, launched_head, started_at, stopped_at,
         context, context_max, last_growth, kicked_at, over_limit_at, transcript
  from sessions
";

/// Register a session that has just started with `agent` and begun writing
/// `transcript`. Its last growth is its start.
#[allow(clippy::too_many_arguments)]
pub fn create<'r>(
  transaction: &Transaction<'_>,
  runtime: &'r dyn SessionRuntime,
  agent_for: fn(AgentKind) -> &'r dyn Agent,
  name: &str,
  role: Role,
  agent: AgentKind,
  external_session_id: &str,
  launched_head: Option<&str>,
  transcript: &Path,
) -> Result<Session<'r>> {
  let started_at = Utc::now();
  let id = transaction.query_row(
    "
      insert into sessions(
        name, role, agent, external_session_id, launched_head, started_at, last_growth,
        transcript
      ) values (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)
      returning id
      ",
    params![
      name,
      role.as_str(),
      agent.as_str(),
      external_session_id,
      launched_head,
      started_at.timestamp_millis(),
      transcript.to_string_lossy(),
    ],
    |row| row.get(0),
  )?;
  get(transaction, runtime, agent_for, id)?
    .with_context(|| format!("created session {id} is missing"))
}

pub fn get<'r>(
  transaction: &Transaction<'_>,
  runtime: &'r dyn SessionRuntime,
  agent_for: fn(AgentKind) -> &'r dyn Agent,
  id: i64,
) -> Result<Option<Session<'r>>> {
  let row = transaction
    .query_row(&format!("{SELECT} where id=?"), [id], session_row)
    .optional()?;
  row
    .map(|row| materialize(row, runtime, agent_for))
    .transpose()
}

/// The newest incarnation of `name`, live or not.
pub fn latest_named<'r>(
  transaction: &Transaction<'_>,
  runtime: &'r dyn SessionRuntime,
  agent_for: fn(AgentKind) -> &'r dyn Agent,
  name: &str,
) -> Result<Option<Session<'r>>> {
  let row = transaction
    .query_row(
      &format!("{SELECT} where name=? order by started_at desc, id desc limit 1"),
      [name],
      session_row,
    )
    .optional()?;
  row
    .map(|row| materialize(row, runtime, agent_for))
    .transpose()
}

pub fn all<'r>(
  transaction: &Transaction<'_>,
  runtime: &'r dyn SessionRuntime,
  agent_for: fn(AgentKind) -> &'r dyn Agent,
) -> Result<Vec<Session<'r>>> {
  let mut statement = transaction.prepare(&format!("{SELECT} order by started_at, id"))?;
  let rows = statement.query_map([], session_row)?;
  rows
    .map(|row| materialize(row?, runtime, agent_for))
    .collect()
}

/// Stop every live incarnation of `name`. Returns how many were stopped.
pub fn stop_named(transaction: &Transaction<'_>, name: &str) -> Result<usize> {
  let stopped = transaction.execute(
    "update sessions set stopped_at=? where name=? and stopped_at is null",
    params![Utc::now().timestamp_millis(), name],
  )?;
  Ok(stopped)
}

/// Remember where the session's transcript was found. It never moves, so
/// nothing ever clears the column.
pub fn record_transcript<'r>(
  transaction: &Transaction<'_>,
  runtime: &'r dyn SessionRuntime,
  agent_for: fn(AgentKind) -> &'r dyn Agent,
  id: i64,
  path: &Path,
) -> Result<Session<'r>> {
  transaction.execute(
    "update sessions set transcript=? where id=?",
    params![path.to_string_lossy(), id],
  )?;
  get(transaction, runtime, agent_for, id)?.with_context(|| format!("session {id} is missing"))
}

/// Record one poll's reading of the transcript. Growth moves the last-growth
/// mark to `at` and re-arms the kick; the maximum only ever rises. A reading
/// whose context is unknown clears the context and leaves the maximum as it
/// was.
pub fn record_reading<'r>(
  transaction: &Transaction<'_>,
  runtime: &'r dyn SessionRuntime,
  agent_for: fn(AgentKind) -> &'r dyn Agent,
  id: i64,
  context: ContextSize,
  grew: bool,
  at: DateTime<Utc>,
) -> Result<Session<'r>> {
  transaction.execute(
    "
      update sessions set
        context=?1,
        context_max=case when ?1 is null then context_max
                         else max(coalesce(context_max, ?1), ?1) end,
        last_growth=case when ?2 then ?3 else last_growth end,
        kicked_at=case when ?2 then null else kicked_at end
      where id=?4
      ",
    params![context.stored(), grew, at.timestamp_millis(), id],
  )?;
  get(transaction, runtime, agent_for, id)?.with_context(|| format!("session {id} is missing"))
}

/// Latch that the session has been nudged; cleared by the next growth.
pub fn record_kick<'r>(
  transaction: &Transaction<'_>,
  runtime: &'r dyn SessionRuntime,
  agent_for: fn(AgentKind) -> &'r dyn Agent,
  id: i64,
) -> Result<Session<'r>> {
  transaction.execute(
    "update sessions set kicked_at=? where id=?",
    params![Utc::now().timestamp_millis(), id],
  )?;
  get(transaction, runtime, agent_for, id)?.with_context(|| format!("session {id} is missing"))
}

/// Stamp that the session crossed its context stop threshold. The update
/// overwrites, so callers check `can_latch_over_limit` first to make it once
/// per session; nothing ever clears the column.
pub fn record_over_limit<'r>(
  transaction: &Transaction<'_>,
  runtime: &'r dyn SessionRuntime,
  agent_for: fn(AgentKind) -> &'r dyn Agent,
  id: i64,
) -> Result<Session<'r>> {
  transaction.execute(
    "update sessions set over_limit_at=? where id=?",
    params![Utc::now().timestamp_millis(), id],
  )?;
  get(transaction, runtime, agent_for, id)?.with_context(|| format!("session {id} is missing"))
}

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
  Ok(SessionRow {
    id: row.get("id")?,
    name: row.get("name")?,
    role: row.get("role")?,
    agent: row.get("agent")?,
    external_session_id: row.get("external_session_id")?,
    launched_head: row.get("launched_head")?,
    started_at: row.get("started_at")?,
    stopped_at: row.get("stopped_at")?,
    context: row.get("context")?,
    context_max: row.get("context_max")?,
    last_growth: row.get("last_growth")?,
    kicked_at: row.get("kicked_at")?,
    over_limit_at: row.get("over_limit_at")?,
    transcript: row.get("transcript")?,
  })
}

fn materialize<'r>(
  row: SessionRow,
  runtime: &'r dyn SessionRuntime,
  agent_for: fn(AgentKind) -> &'r dyn Agent,
) -> Result<Session<'r>> {
  let on_session = |error: anyhow::Error| anyhow!("session {}: {error}", row.name);
  let role = Role::try_from(row.role.as_str()).map_err(on_session)?;
  let agent_kind = AgentKind::try_from(row.agent.as_str()).map_err(on_session)?;
  Session::new(
    runtime,
    agent_for(agent_kind),
    row.id,
    row.name,
    role,
    agent_kind,
    row.external_session_id,
    row.launched_head,
    time(row.started_at, "started_at")?,
    row
      .stopped_at
      .map(|at| time(at, "stopped_at"))
      .transpose()?,
    ContextSize::from_stored(row.context)?,
    ContextSize::from_stored(row.context_max)?,
    time(row.last_growth, "last_growth")?,
    row.kicked_at.map(|at| time(at, "kicked_at")).transpose()?,
    row
      .over_limit_at
      .map(|at| time(at, "over_limit_at"))
      .transpose()?,
    row.transcript.map(From::from),
  )
}

fn time(millis: i64, field: &str) -> Result<DateTime<Utc>> {
  DateTime::from_timestamp_millis(millis)
    .with_context(|| format!("session {field} is outside the supported range"))
}

#[cfg(test)]
mod tests;
