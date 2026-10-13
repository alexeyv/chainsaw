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

  #[cfg(windows)]
  #[test]
  fn should_replace_the_colon_and_backslashes_when_the_run_directory_is_on_windows() {
    assert_eq!(state_dir_name(Path::new(r"C:\x\y")), "C--x-y");
  }

  #[cfg(unix)]
  #[test]
  fn should_keep_colons_and_backslashes_when_the_run_directory_is_on_posix() {
    assert_eq!(state_dir_name(Path::new(r"/tmp/a:b\c")), r"-tmp-a:b\c");
  }
}

mod runtime_named_by {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(runtime_named_by(false, None).unwrap(), None);
  }

  #[cfg(unix)]
  #[test]
  fn should_choose_orca_when_inside_an_orca_terminal_only() {
    assert_eq!(
      runtime_named_by(false, Some("term-1".to_owned())).unwrap(),
      Some("term-1".to_owned())
    );
  }

  #[test]
  fn should_choose_herdr_when_the_herdr_pane_was_opened_from_an_orca_terminal() {
    assert_eq!(
      runtime_named_by(true, Some("term-1".to_owned())).unwrap(),
      None
    );
  }

  #[cfg(windows)]
  #[test]
  fn should_fail_naming_herdr_when_orca_is_chosen_on_windows() {
    assert_eq!(
      runtime_named_by(false, Some("term-1".to_owned()))
        .unwrap_err()
        .to_string(),
      "supervisor: Orca is not supported on Windows; run chainsaw from a Herdr pane"
    );
  }
}
