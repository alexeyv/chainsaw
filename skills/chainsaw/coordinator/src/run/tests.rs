use std::path::Path;

use super::{runtime_named_by, state_dir_name};

mod state_dir_name {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      state_dir_name(Path::new("/Users/alex/src/run")),
      "-Users-alex-src-run"
    );
  }

  #[test]
  fn should_replace_dots_as_well_as_separators() {
    assert_eq!(
      state_dir_name(Path::new("/Users/alex/src/ui.wt/run")),
      "-Users-alex-src-ui-wt-run"
    );
  }

  #[test]
  fn should_replace_a_dot_that_starts_a_component() {
    assert_eq!(state_dir_name(Path::new("/x/.bare")), "-x--bare");
  }
}

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
