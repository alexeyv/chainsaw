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
