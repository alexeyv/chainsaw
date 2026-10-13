use super::one_line;

mod one_line {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      one_line("Read these files.\nThen stop."),
      "Read these files. Then stop."
    );
  }

  #[test]
  fn should_leave_a_prompt_alone_when_it_is_on_one_line() {
    assert_eq!(one_line("Read the brief."), "Read the brief.");
  }

  #[test]
  fn should_drop_blank_lines_tabs_and_carriage_returns_when_the_prompt_has_them() {
    assert_eq!(
      one_line("1. Read.\r\n\r\n\t2. Stop.\n"),
      "1. Read. 2. Stop."
    );
  }
}
