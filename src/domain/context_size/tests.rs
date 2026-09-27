use super::ContextSize;

mod from_stored {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      ContextSize::from_stored(Some(4_000)).unwrap(),
      ContextSize::tokens(4_000)
    );
  }

  #[test]
  fn should_read_null_as_unknown() {
    assert_eq!(
      ContextSize::from_stored(None).unwrap(),
      ContextSize::UNKNOWN
    );
  }

  #[test]
  fn should_fail_when_the_count_is_negative() {
    let error = ContextSize::from_stored(Some(-1)).unwrap_err();
    assert_eq!(error.to_string(), "context size cannot be negative");
  }
}

mod exceeds {
  use super::*;

  #[test]
  fn should_work() {
    assert!(ContextSize::tokens(101).exceeds(100));
  }

  #[test]
  fn should_not_count_the_limit_itself() {
    assert!(!ContextSize::tokens(100).exceeds(100));
  }

  #[test]
  fn should_never_be_exceeded_by_an_unknown_reading() {
    assert!(!ContextSize::UNKNOWN.exceeds(0));
  }
}

mod is_under {
  use super::*;

  #[test]
  fn should_work() {
    assert!(ContextSize::tokens(99).is_under(100));
  }

  #[test]
  fn should_not_count_the_limit_itself() {
    assert!(!ContextSize::tokens(100).is_under(100));
  }

  #[test]
  fn should_never_hold_for_an_unknown_reading() {
    assert!(!ContextSize::UNKNOWN.is_under(u64::MAX));
  }
}

mod or {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      ContextSize::tokens(5).or(ContextSize::tokens(9)),
      ContextSize::tokens(5)
    );
  }

  #[test]
  fn should_fall_back_when_the_reading_is_unknown() {
    assert_eq!(
      ContextSize::UNKNOWN.or(ContextSize::tokens(9)),
      ContextSize::tokens(9)
    );
  }
}

mod since {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(
      ContextSize::tokens(900).since(ContextSize::tokens(100)),
      ContextSize::tokens(800)
    );
  }

  #[test]
  fn should_read_zero_when_the_context_shrank() {
    assert_eq!(
      ContextSize::tokens(100).since(ContextSize::tokens(900)),
      ContextSize::tokens(0)
    );
  }

  #[test]
  fn should_be_unknown_when_either_reading_is() {
    assert_eq!(
      ContextSize::tokens(900).since(ContextSize::UNKNOWN),
      ContextSize::UNKNOWN
    );
    assert_eq!(
      ContextSize::UNKNOWN.since(ContextSize::tokens(100)),
      ContextSize::UNKNOWN
    );
  }
}

mod display {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(ContextSize::tokens(4_000).to_string(), "4000");
  }

  #[test]
  fn should_say_unknown() {
    assert_eq!(ContextSize::UNKNOWN.to_string(), "unknown");
  }

  #[test]
  fn should_honour_a_width() {
    assert_eq!(format!("{:>7}", ContextSize::tokens(42)), "     42");
    assert_eq!(format!("{:>7}|", ContextSize::UNKNOWN), "unknown|");
  }
}
