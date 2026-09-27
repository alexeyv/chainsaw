use super::*;

mod runtime_named_by {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      runtime_named_by(false, Some("term-1".to_owned())),
      Some("term-1".to_owned())
    );
  }

  #[test]
  fn should_choose_herdr_when_the_herdr_pane_was_opened_from_an_orca_terminal() {
    assert_eq!(runtime_named_by(true, Some("term-1".to_owned())), None);
  }

  #[test]
  fn should_choose_herdr_outside_both() {
    assert_eq!(runtime_named_by(false, None), None);
  }
}
