use anyhow::Result;
use chrono::{DateTime, Utc};

fn millisecond_floor(time: DateTime<Utc>) -> DateTime<Utc> {
  DateTime::from_timestamp_millis(time.timestamp_millis()).unwrap()
}

use super::{after, create};
use crate::domain::Observation;
use crate::domain::test_helpers::format_observation;
use crate::persistence::test_fixture::{database, row_count, task_row};

fn format_observations(observations: &[Observation]) -> String {
  observations
    .iter()
    .map(format_observation)
    .collect::<Vec<_>>()
    .join("\n\n")
}

mod create {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;

    let before = millisecond_floor(Utc::now());
    let transaction = db.transaction()?;
    let observation = create(&transaction, Some(5), "the gate ran twice")?;
    transaction.commit()?;
    let after = Utc::now();

    let stored = db.query_row(
      "select task_id, text, created_at from observations where id=?",
      [observation.id()],
      |row| {
        Ok((
          row.get::<_, Option<i64>>(0)?,
          row.get::<_, String>(1)?,
          row.get::<_, i64>(2)?,
        ))
      },
    )?;
    assert_eq!(observation.id(), 1);
    assert_eq!(observation.task_id(), Some(5));
    assert_eq!(observation.text(), "the gate ran twice");
    assert!(observation.created_at() >= before);
    assert!(observation.created_at() <= after);
    assert_eq!(
      stored,
      (
        Some(5),
        "the gate ran twice".to_owned(),
        observation.created_at().timestamp_millis()
      )
    );
    Ok(())
  }

  #[test]
  fn should_record_a_run_wide_observation_with_no_task() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let observation = create(&transaction, None, "run wide")?;
    transaction.commit()?;

    assert_eq!(observation.task_id(), None);
    assert_eq!(row_count(&db, "observations")?, 1);
    Ok(())
  }

  #[test]
  fn should_leave_commit_and_rollback_to_the_caller() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    create(&transaction, None, "temporary")?;
    transaction.rollback()?;

    assert_eq!(row_count(&db, "observations")?, 0);
    Ok(())
  }

  #[test]
  fn should_fail_when_the_task_does_not_exist() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(&transaction, Some(9), "observation").unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    Ok(())
  }

  #[test]
  fn should_fail_when_the_text_is_blank() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let error = create(&transaction, None, " ").unwrap_err();
    transaction.rollback()?;

    assert_eq!(error.to_string(), "text cannot be blank");
    Ok(())
  }
}

mod after {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    task_row(&db, 5)?;
    let transaction = db.transaction()?;
    let first = create(&transaction, Some(5), "first")?;
    let second = create(&transaction, None, "second")?;
    let third = create(&transaction, Some(5), "third")?;
    transaction.commit()?;

    assert_eq!(
      format_observations(&after(&db.unchecked_transaction()?, first.id(), None)?),
      format_observations(&[second, third])
    );
    Ok(())
  }

  #[test]
  fn should_return_everything_after_zero() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let first = create(&transaction, None, "first")?;
    let second = create(&transaction, None, "second")?;
    transaction.commit()?;

    assert_eq!(
      format_observations(&after(&db.unchecked_transaction()?, 0, None)?),
      format_observations(&[first, second])
    );
    Ok(())
  }

  #[test]
  fn should_return_nothing_when_there_is_nothing_newer() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let only = create(&transaction, None, "only")?;
    transaction.commit()?;

    assert_eq!(
      after(&db.unchecked_transaction()?, only.id(), None)?,
      Vec::new()
    );
    Ok(())
  }

  #[test]
  fn should_include_run_wide_observations_but_not_other_tasks_when_filtering_by_task() -> Result<()>
  {
    let mut db = database();
    task_row(&db, 5)?;
    task_row(&db, 6)?;
    let transaction = db.transaction()?;
    let relevant = create(&transaction, Some(5), "relevant")?;
    create(&transaction, Some(6), "other task")?;
    let run_wide = create(&transaction, None, "run wide")?;
    transaction.commit()?;

    assert_eq!(
      format_observations(&after(&db.unchecked_transaction()?, 0, Some(5))?),
      format_observations(&[relevant, run_wide])
    );
    Ok(())
  }
}
