use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use super::require_positive;

/// Informational chronological context that requires no response.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
  id: i64,
  task_id: Option<i64>,
  text: String,
  created_at: DateTime<Utc>,
}

impl Observation {
  pub fn new(
    id: i64,
    task_id: Option<i64>,
    text: String,
    created_at: DateTime<Utc>,
  ) -> Result<Self> {
    require_positive("id", id)?;
    if let Some(task_id) = task_id {
      require_positive("task_id", task_id)?;
    }
    if text.trim().is_empty() {
      bail!("text cannot be blank");
    }
    Ok(Self {
      id,
      task_id,
      text,
      created_at,
    })
  }

  pub fn id(&self) -> i64 {
    self.id
  }

  pub fn task_id(&self) -> Option<i64> {
    self.task_id
  }

  pub fn text(&self) -> &str {
    &self.text
  }

  pub fn created_at(&self) -> DateTime<Utc> {
    self.created_at
  }
}

#[cfg(test)]
mod tests {
  use chrono::{DateTime, Utc};

  use super::Observation;

  fn timestamp() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap()
  }

  #[test]
  fn exposes_valid_informational_context() {
    let observation =
      Observation::new(3, Some(5), "review complete".to_owned(), timestamp()).unwrap();

    assert_eq!(observation.id(), 3);
    assert_eq!(observation.task_id(), Some(5));
    assert_eq!(observation.text(), "review complete");
    assert_eq!(observation.created_at(), timestamp());
  }

  #[test]
  fn validates_identity_task_and_text() {
    assert_eq!(
      Observation::new(0, None, "context".to_owned(), timestamp())
        .unwrap_err()
        .to_string(),
      "id must be positive"
    );
    assert_eq!(
      Observation::new(1, Some(0), "context".to_owned(), timestamp())
        .unwrap_err()
        .to_string(),
      "task_id must be positive"
    );
    assert_eq!(
      Observation::new(1, None, " ".to_owned(), timestamp())
        .unwrap_err()
        .to_string(),
      "text cannot be blank"
    );
  }
}
