use crate::domain::test_helpers::{format_prompt, prompt};

mod new {
  use super::*;

  #[test]
  fn should_work() {
    let prompt = prompt(3, 5, "continue").unwrap();

    assert_eq!(
      format_prompt(&prompt),
      r#"id: 3
session_id: 5
text: "continue"
sent_at: 2023-11-14T22:13:20Z
seen_at: none
attempts: 0"#
    );
  }

  #[test]
  fn should_fail_when_the_id_is_not_positive() {
    for id in [i64::MIN, -1, 0] {
      let error = prompt(id, 5, "continue").unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_session_id_is_not_positive() {
    for session_id in [i64::MIN, -1, 0] {
      let error = prompt(3, session_id, "continue").unwrap_err();
      assert_eq!(error.to_string(), "session_id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_text_is_blank() {
    for text in ["", " ", "\n\t"] {
      let error = prompt(3, 5, text).unwrap_err();
      assert_eq!(error.to_string(), "text cannot be blank");
    }
  }

  #[test]
  fn should_fail_when_the_attempts_are_negative() {
    use crate::domain::Prompt;
    use crate::domain::test_helpers::created_at;

    let error = Prompt::new(3, 5, "continue".to_owned(), created_at(), None, -1).unwrap_err();

    assert_eq!(error.to_string(), "attempts cannot be negative");
  }
}
