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
  log_offset: i64,
  base_head: Option<String>,
  predicted_file_list: Option<Vec<String>>,
  is_session_reuse: bool,
  context_size_start: Option<i64>,
}

const SELECT: &str = "
  select id, text, predicted_files, predicted_lines, session_id,
         commit_sha, created_at, retry_of_task_id, log_offset,
         base_head, predicted_file_list, is_session_reuse,
         context_size_start
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
  log_offset: i64,
  reuse: bool,
  reason: Option<&str>,
) -> Result<Task> {
  advance(
    transaction,
    id,
    TaskState::Dispatched,
    reason,
    |transaction| {
      transaction.execute(
        "update tasks set session_id=?, log_offset=?, is_session_reuse=? where id=?",
        params![session_id, log_offset, i64::from(reuse), id],
      )?;
      Ok(())
    },
  )
}

/// The dispatch `log_offset` stays as the measurement baseline.
pub fn take_flight(
  transaction: &Transaction<'_>,
  id: i64,
  base_head: &str,
  context_size_start: i64,
) -> Result<Task> {
  advance(transaction, id, TaskState::InFlight, None, |transaction| {
    transaction.execute(
      "update tasks set base_head=?, context_size_start=? where id=?",
      params![base_head, context_size_start, id],
    )?;
    Ok(())
  })
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
  advance(transaction, id, TaskState::Accepted, Some(reason), |_| {
    Ok(())
  })
}

pub fn abort(transaction: &Transaction<'_>, id: i64, reason: &str) -> Result<Task> {
  advance(
    transaction,
    id,
    TaskState::Aborted,
    Some(reason),
    |_| Ok(()),
  )
}

/// Move a task forward to `next`, recording an optional reason for the move.
/// Advancing to the state a task already occupies is a no-op that succeeds, so
/// callers that re-observe the same fact do not have to guard against it.
pub fn advance(
  transaction: &Transaction<'_>,
  id: i64,
  next: TaskState,
  reason: Option<&str>,
  mutate: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<Task> {
  let current = get(transaction, id)?.with_context(|| format!("task {id} is missing"))?;
  if current.state() == next {
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
    log_offset: row.get::<_, Option<i64>>("log_offset")?.unwrap_or_default(),
    base_head: row.get("base_head")?,
    predicted_file_list,
    is_session_reuse: row.get::<_, i64>("is_session_reuse")? != 0,
    context_size_start: row.get("context_size_start")?,
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
    DateTime::from_timestamp_millis(row.created_at)
      .context("task created_at is outside the supported range")?,
    row.retry_of_task_id,
    row.log_offset,
    row.base_head,
    row.predicted_file_list,
    row.is_session_reuse,
    row.context_size_start,
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
      let created_at = DateTime::from_timestamp_millis(created_at)
        .context("task event created_at is outside the supported range")?;
      TaskEvent::new(id, state, reason, created_at)
    })
    .collect()
}

#[cfg(test)]
mod tests;
