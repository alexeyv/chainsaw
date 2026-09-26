use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::domain::{Task, TaskEvent, TaskState};

struct TaskRow {
  id: i64,
  text: String,
  predicted_files: i64,
  predicted_lines: i64,
  session_id: Option<i64>,
  commit_sha: Option<String>,
  created_at: i64,
  retry_of_task_id: Option<i64>,
  transcript_offset: i64,
  base_head: Option<String>,
  predicted_file_list: Option<Vec<String>>,
  context_size_start: Option<i64>,
  commentary_requested_at: Option<i64>,
  commentary_delivered_at: Option<i64>,
}

const SELECT: &str = "
  select id, text, predicted_files, predicted_lines, session_id,
         commit_sha, created_at, retry_of_task_id, transcript_offset,
         base_head, predicted_file_list, context_size_start,
         commentary_requested_at, commentary_delivered_at
  from tasks
";

/// Predicted file names are stored joined by this; a name containing it would
/// split into two on the way back and make the task unreadable.
const FILE_LIST_SEPARATOR: char = ',';

/// Inserts first because `Task` needs the ids the insert returns; the final
/// `get` validates, and a failure aborts the caller's transaction, so no
/// unvalidated task is ever visible.
pub fn create(
  transaction: &Transaction<'_>,
  text: &str,
  predicted_files: i64,
  predicted_lines: i64,
  retry_of_task_id: Option<i64>,
  predicted_file_list: Option<Vec<String>>,
) -> Result<Task> {
  if let Some(file) = predicted_file_list
    .iter()
    .flatten()
    .find(|file| file.contains(FILE_LIST_SEPARATOR))
  {
    bail!("predicted file name {file:?} contains {FILE_LIST_SEPARATOR:?}");
  }
  // An empty list would come back as one blank name; store it as no list.
  let predicted_file_list = predicted_file_list.filter(|files| !files.is_empty());
  let created_at = Utc::now().timestamp_millis();
  let stored_file_list = predicted_file_list
    .as_ref()
    .map(|files| files.join(&FILE_LIST_SEPARATOR.to_string()));
  let id = transaction.query_row(
    "
      insert into tasks(
        text, predicted_files, predicted_lines, created_at, retry_of_task_id,
        predicted_file_list
      ) values (?1, ?2, ?3, ?4, ?5, ?6)
      returning id
      ",
    params![
      text,
      predicted_files,
      predicted_lines,
      created_at,
      retry_of_task_id,
      stored_file_list,
    ],
    |row| row.get(0),
  )?;
  super::task_event::create(transaction, id, TaskState::Drafted, None)?;
  get(transaction, id)?.with_context(|| format!("created task {id} is missing"))
}

pub fn get(transaction: &Transaction<'_>, id: i64) -> Result<Option<Task>> {
  let row = transaction
    .query_row(&format!("{SELECT} where id=?"), [id], task_row)
    .optional()?;
  row.map(|row| materialize(transaction, row)).transpose()
}

pub fn all(transaction: &Transaction<'_>) -> Result<Vec<Task>> {
  let rows = {
    let mut statement = transaction.prepare(&format!("{SELECT} order by id asc"))?;
    statement
      .query_map([], task_row)?
      .collect::<rusqlite::Result<Vec<_>>>()?
  };
  rows
    .into_iter()
    .map(|row| materialize(transaction, row))
    .collect()
}

pub fn tasks_for_session(transaction: &Transaction<'_>, session_id: i64) -> Result<Vec<Task>> {
  let rows = {
    let mut statement =
      transaction.prepare(&format!("{SELECT} where session_id=? order by id asc"))?;
    statement
      .query_map([session_id], task_row)?
      .collect::<rusqlite::Result<Vec<_>>>()?
  };
  rows
    .into_iter()
    .map(|row| materialize(transaction, row))
    .collect()
}

pub fn predecessor(transaction: &Transaction<'_>, id: i64) -> Result<Option<Task>> {
  let row = transaction
    .query_row(
      &format!("{SELECT} where id < ? order by id desc limit 1"),
      [id],
      task_row,
    )
    .optional()?;
  row.map(|row| materialize(transaction, row)).transpose()
}

pub fn dispatch(
  transaction: &Transaction<'_>,
  id: i64,
  session_id: i64,
  transcript_offset: i64,
  reason: Option<&str>,
) -> Result<Task> {
  advance(
    transaction,
    id,
    TaskState::Dispatched,
    reason,
    |current| {
      same_fact(current, "session", current.session_id(), Some(session_id))?;
      same_fact(
        current,
        "transcript offset",
        current.transcript_offset(),
        transcript_offset,
      )
    },
    |transaction| {
      transaction.execute(
        "update tasks set session_id=?, transcript_offset=? where id=?",
        params![session_id, transcript_offset, id],
      )?;
      Ok(())
    },
  )
}

/// The dispatch `transcript_offset` stays as the measurement baseline.
pub fn take_flight(
  transaction: &Transaction<'_>,
  id: i64,
  base_head: &str,
  context_size_start: i64,
) -> Result<Task> {
  advance(
    transaction,
    id,
    TaskState::InFlight,
    None,
    |current| {
      same_fact(current, "base head", current.base_head(), Some(base_head))?;
      same_fact(
        current,
        "context size start",
        current.context_size_start(),
        Some(context_size_start),
      )
    },
    |transaction| {
      transaction.execute(
        "update tasks set base_head=?, context_size_start=? where id=?",
        params![base_head, context_size_start, id],
      )?;
      Ok(())
    },
  )
}

pub fn record_commit(
  transaction: &Transaction<'_>,
  id: i64,
  commit_sha: &str,
  reason: Option<&str>,
) -> Result<Task> {
  advance(
    transaction,
    id,
    TaskState::CommittedUnverified,
    reason,
    |current| same_fact(current, "commit", current.commit_sha(), Some(commit_sha)),
    |transaction| {
      transaction.execute(
        "update tasks set commit_sha=? where id=?",
        params![commit_sha, id],
      )?;
      Ok(())
    },
  )
}

pub fn accept(transaction: &Transaction<'_>, id: i64, reason: &str) -> Result<Task> {
  advance(
    transaction,
    id,
    TaskState::Accepted,
    Some(reason),
    |_| Ok(()),
    |_| Ok(()),
  )
}

pub fn abort(transaction: &Transaction<'_>, id: i64, reason: &str) -> Result<Task> {
  advance(
    transaction,
    id,
    TaskState::Aborted,
    Some(reason),
    |_| Ok(()),
    |_| Ok(()),
  )
}

/// Stamp the moment commentary on the task's commit was first requested from
/// the commentator. Returns `false` when a request is already recorded; the
/// first stamp stands.
pub fn record_commentary_request(transaction: &Transaction<'_>, id: i64) -> Result<bool> {
  record_commentary_stamp(transaction, id, "commentary_requested_at")
}

/// Stamp the moment the commentator's review of the task's commit was first
/// observed. Returns `false` when a delivery is already recorded.
pub fn record_commentary_delivery(transaction: &Transaction<'_>, id: i64) -> Result<bool> {
  record_commentary_stamp(transaction, id, "commentary_delivered_at")
}

/// Write-once: the column is set only while it is still null, and only on a
/// task that has a commit for the commentator to review.
fn record_commentary_stamp(transaction: &Transaction<'_>, id: i64, column: &str) -> Result<bool> {
  let current = get(transaction, id)?.with_context(|| format!("task {id} is missing"))?;
  if !matches!(
    current.state(),
    TaskState::CommittedUnverified | TaskState::Accepted
  ) || current.commit_sha().is_none()
  {
    bail!("task {id} is {}, not ready for commentary", current.state());
  }
  let changed = transaction.execute(
    &format!("update tasks set {column}=? where id=? and {column} is null"),
    params![Utc::now().timestamp_millis(), id],
  )?;
  Ok(changed == 1)
}

/// Move a task forward to `next`, recording an optional reason for the move.
/// Advancing to the state a task already occupies is idempotent: `same_fact`
/// checks that what the caller re-observed is what was recorded, and then
/// nothing is written; a differing observation is an error, never overwritten
/// or dropped.
fn advance(
  transaction: &Transaction<'_>,
  id: i64,
  next: TaskState,
  reason: Option<&str>,
  same_fact: impl FnOnce(&Task) -> Result<()>,
  mutate: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<Task> {
  let current = get(transaction, id)?.with_context(|| format!("task {id} is missing"))?;
  if current.state() == next {
    self::same_fact(&current, "reason", current.reason(), reason)?;
    same_fact(&current)?;
    return Ok(current);
  }
  if !current.state().can_transition_to(next) {
    bail!(
      "task {id} cannot advance from {} to {next}",
      current.state()
    );
  }
  mutate(transaction)?;
  super::task_event::create(transaction, id, next, reason)?;
  get(transaction, id)?.with_context(|| format!("task {id} disappeared while becoming {next}"))
}

fn same_fact<T: PartialEq + std::fmt::Debug>(
  current: &Task,
  what: &str,
  recorded: T,
  observed: T,
) -> Result<()> {
  if recorded != observed {
    bail!(
      "task {} is already {} with {what} {recorded:?}, not {observed:?}",
      current.id(),
      current.state()
    );
  }
  Ok(())
}

fn task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
  let predicted_file_list = row
    .get::<_, Option<String>>("predicted_file_list")?
    .map(|files| {
      files
        .split(FILE_LIST_SEPARATOR)
        .map(str::to_owned)
        .collect()
    });
  Ok(TaskRow {
    id: row.get("id")?,
    text: row.get("text")?,
    predicted_files: row.get("predicted_files")?,
    predicted_lines: row.get("predicted_lines")?,
    session_id: row.get("session_id")?,
    commit_sha: row.get("commit_sha")?,
    created_at: row.get("created_at")?,
    retry_of_task_id: row.get("retry_of_task_id")?,
    transcript_offset: row
      .get::<_, Option<i64>>("transcript_offset")?
      .unwrap_or_default(),
    base_head: row.get("base_head")?,
    predicted_file_list,
    context_size_start: row.get("context_size_start")?,
    commentary_requested_at: row.get("commentary_requested_at")?,
    commentary_delivered_at: row.get("commentary_delivered_at")?,
  })
}

fn materialize(transaction: &Transaction<'_>, row: TaskRow) -> Result<Task> {
  let events = load_events(transaction, row.id)?;
  Task::new(
    row.id,
    row.text,
    row.predicted_files,
    row.predicted_lines,
    row.session_id,
    row.commit_sha,
    time(row.created_at, "created_at")?,
    row.retry_of_task_id,
    row.transcript_offset,
    row.base_head,
    row.predicted_file_list,
    row.context_size_start,
    row
      .commentary_requested_at
      .map(|millis| time(millis, "commentary_requested_at"))
      .transpose()?,
    row
      .commentary_delivered_at
      .map(|millis| time(millis, "commentary_delivered_at"))
      .transpose()?,
    events,
  )
}

fn load_events(transaction: &Transaction<'_>, task_id: i64) -> Result<Vec<TaskEvent>> {
  let mut statement = transaction.prepare(
    "
      select id, state, reason, created_at
      from task_events where task_id=? order by id
      ",
  )?;
  let rows = statement.query_map([task_id], |row| {
    Ok((
      row.get::<_, i64>(0)?,
      row.get::<_, String>(1)?,
      row.get::<_, Option<String>>(2)?,
      row.get::<_, i64>(3)?,
    ))
  })?;
  rows
    .map(|row| {
      let (id, state, reason, created_at) = row?;
      let state = TaskState::try_from(state.as_str())?;
      let created_at = time(created_at, "event created_at")?;
      TaskEvent::new(id, state, reason, created_at)
    })
    .collect()
}

fn time(millis: i64, field: &str) -> Result<DateTime<Utc>> {
  DateTime::from_timestamp_millis(millis)
    .with_context(|| format!("task {field} is outside the supported range"))
}

#[cfg(test)]
mod tests;
