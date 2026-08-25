use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Transaction, params};

use crate::domain::Observation;

pub fn create(
  transaction: &Transaction<'_>,
  task_id: Option<i64>,
  text: &str,
) -> Result<Observation> {
  let created_at = Utc::now().timestamp_millis();
  let id = transaction.query_row(
    "
      insert into observations(task_id, text, created_at)
      values (?1, ?2, ?3) returning id
      ",
    params![task_id, text, created_at],
    |row| row.get(0),
  )?;
  materialize(id, task_id, text.to_owned(), created_at)
}

pub fn after(
  db: &Connection,
  observation_id: i64,
  task_id: Option<i64>,
) -> Result<Vec<Observation>> {
  let mut statement = db.prepare(
    "
      select id, task_id, text, created_at
      from observations
      where id > ?1 and (?2 is null or task_id is null or task_id = ?2)
      order by id
      ",
  )?;
  let rows = statement.query_map(params![observation_id, task_id], |row| {
    Ok((
      row.get::<_, i64>(0)?,
      row.get::<_, Option<i64>>(1)?,
      row.get::<_, String>(2)?,
      row.get::<_, i64>(3)?,
    ))
  })?;
  rows
    .map(|row| {
      let (id, task_id, text, created_at) = row?;
      materialize(id, task_id, text, created_at)
    })
    .collect()
}

fn materialize(
  id: i64,
  task_id: Option<i64>,
  text: String,
  created_at: i64,
) -> Result<Observation> {
  let created_at = DateTime::from_timestamp_millis(created_at)
    .context("observation created_at is outside the supported range")?;
  Observation::new(id, task_id, text, created_at)
}

#[cfg(test)]
mod tests {
  use anyhow::Result;

  use super::{after, create};
  use crate::persistence::test_fixture::database;

  #[test]
  fn records_and_incrementally_reads_chronological_observations() -> Result<()> {
    let mut db = database();
    db.execute("insert into tasks(id) values(5)", [])?;

    let transaction = db.transaction()?;
    let first = create(&transaction, Some(5), "first")?;
    let second = create(&transaction, None, "second")?;
    transaction.commit()?;

    assert_eq!(first.id(), 1);
    assert_eq!(first.task_id(), Some(5));
    assert_eq!(first.text(), "first");
    assert_eq!(after(&db, first.id(), None)?, vec![second]);
    Ok(())
  }

  #[test]
  fn task_filter_includes_run_wide_observations_but_not_other_tasks() -> Result<()> {
    let mut db = database();
    db.execute_batch("insert into tasks(id) values(5); insert into tasks(id) values(6);")?;
    let transaction = db.transaction()?;
    let relevant = create(&transaction, Some(5), "relevant")?;
    create(&transaction, Some(6), "other task")?;
    let run_wide = create(&transaction, None, "run wide")?;
    transaction.commit()?;

    assert_eq!(after(&db, 0, Some(5))?, vec![relevant, run_wide]);
    Ok(())
  }

  #[test]
  fn rejects_a_missing_task_reference() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let task_error = create(&transaction, Some(9), "observation").unwrap_err();
    transaction.rollback()?;

    assert_eq!(task_error.to_string(), "FOREIGN KEY constraint failed");
    Ok(())
  }
}
