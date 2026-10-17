use super::*;
use crate::domain::test_helpers::{
  busy, commentator_at, commentator_over_the_line, idle, never_asked, self_compacting_watch,
  unmeasured_commentator, watch_asked_to_stop, watch_compacting, watch_interrupted,
};

mod poll {
  use super::*;

  #[test]
  fn should_work() {
    let mut watch = self_compacting_watch();

    // It works its way up to the line and compacts itself between turns.
    assert_eq!(
      watch.poll(commentator_at(90_000), never_asked),
      CommentatorAction::Wait
    );
    assert_eq!(
      watch.poll(commentator_at(138_000), never_asked),
      CommentatorAction::Wait
    );
    assert_eq!(
      watch.poll(commentator_at(21_000), never_asked),
      CommentatorAction::Recover
    );
    assert_eq!(
      watch.poll(commentator_at(24_000), never_asked),
      CommentatorAction::Wait
    );
  }

  #[test]
  fn should_recover_again_when_it_compacts_a_second_time() {
    let mut watch = self_compacting_watch();
    watch.poll(commentator_at(138_000), never_asked);
    watch.poll(commentator_at(21_000), never_asked);
    watch.poll(commentator_at(139_000), never_asked);

    assert_eq!(
      watch.poll(commentator_at(19_000), never_asked),
      CommentatorAction::Recover
    );
  }

  #[test]
  fn should_recover_when_the_first_reading_after_a_daemon_restart_is_the_drop() {
    let mut watch = CommentatorWatch::new(true, ContextSize::tokens(137_000));

    assert_eq!(
      watch.poll(commentator_at(20_000), never_asked),
      CommentatorAction::Recover
    );
  }

  #[test]
  fn should_remember_the_last_known_reading_across_an_unknown_one() {
    let mut watch = self_compacting_watch();
    watch.poll(commentator_at(138_000), never_asked);

    assert_eq!(
      watch.poll(unmeasured_commentator(0, 60), never_asked),
      CommentatorAction::Wait
    );
    assert_eq!(
      watch.poll(commentator_at(20_000), never_asked),
      CommentatorAction::Recover
    );
  }

  #[test]
  fn should_not_take_a_small_drop_for_a_compaction() {
    let mut watch = self_compacting_watch();
    watch.poll(commentator_at(138_000), never_asked);

    assert_eq!(
      watch.poll(commentator_at(70_000), never_asked),
      CommentatorAction::Wait
    );
  }

  #[test]
  fn should_not_take_a_drop_from_a_small_context_for_a_compaction() {
    let mut watch = CommentatorWatch::new(true, ContextSize::tokens(COMPACTED_FROM_TOKENS - 1));

    assert_eq!(
      watch.poll(commentator_at(0), never_asked),
      CommentatorAction::Wait
    );
  }

  #[test]
  fn should_ask_to_stop_when_the_context_stays_over_the_line_for_several_polls() {
    let mut watch = self_compacting_watch();
    for _ in 1..OVER_LINE_POLLS {
      assert_eq!(
        watch.poll(commentator_over_the_line(), never_asked),
        CommentatorAction::Wait
      );
    }

    assert_eq!(
      watch.poll(commentator_over_the_line(), never_asked),
      CommentatorAction::AskToStop
    );
  }

  #[test]
  fn should_count_only_polls_over_the_line_in_a_row() {
    let mut watch = self_compacting_watch();
    for _ in 1..OVER_LINE_POLLS {
      watch.poll(commentator_over_the_line(), never_asked);
    }
    watch.poll(commentator_at(COMMENTATOR_COMPACT_TOKENS), never_asked);

    for _ in 1..OVER_LINE_POLLS {
      assert_eq!(
        watch.poll(commentator_over_the_line(), never_asked),
        CommentatorAction::Wait
      );
    }
  }

  #[test]
  fn should_wait_while_a_commentator_asked_to_stop_finishes_its_turn() {
    let mut watch = watch_asked_to_stop();

    assert_eq!(
      watch.poll(commentator_over_the_line(), busy),
      CommentatorAction::Wait
    );
  }

  #[test]
  fn should_compact_a_commentator_asked_to_stop_once_it_is_idle() {
    let mut watch = watch_asked_to_stop();
    watch.poll(commentator_over_the_line(), busy);

    assert_eq!(
      watch.poll(commentator_over_the_line(), idle),
      CommentatorAction::Compact
    );
  }

  #[test]
  fn should_interrupt_a_commentator_that_stays_busy_after_it_was_asked_to_stop() {
    let mut watch = watch_asked_to_stop();
    for _ in 1..STOP_GRACE_POLLS {
      assert_eq!(
        watch.poll(commentator_over_the_line(), busy),
        CommentatorAction::Wait
      );
    }

    assert_eq!(
      watch.poll(commentator_over_the_line(), busy),
      CommentatorAction::Interrupt
    );
  }

  #[test]
  fn should_compact_an_interrupted_commentator_once_it_is_idle() {
    let mut watch = watch_interrupted();

    assert_eq!(
      watch.poll(commentator_over_the_line(), idle),
      CommentatorAction::Compact
    );
  }

  #[test]
  fn should_restart_a_commentator_that_stays_busy_after_its_interrupt() {
    let mut watch = watch_interrupted();
    for _ in 1..STOP_GRACE_POLLS {
      assert_eq!(
        watch.poll(commentator_over_the_line(), busy),
        CommentatorAction::Wait
      );
    }

    assert_eq!(
      watch.poll(commentator_over_the_line(), busy),
      CommentatorAction::Restart
    );
  }

  #[test]
  fn should_recover_a_commentator_that_compacts_itself_when_asked_to_stop() {
    let mut watch = watch_asked_to_stop();

    assert_eq!(
      watch.poll(commentator_at(20_000), never_asked),
      CommentatorAction::Recover
    );
    assert!(!watch.holds_commits());
  }

  #[test]
  fn should_recover_when_the_compaction_takes() {
    let mut watch = watch_compacting();
    assert_eq!(
      watch.poll(commentator_over_the_line(), never_asked),
      CommentatorAction::Wait
    );

    assert_eq!(
      watch.poll(commentator_at(20_000), never_asked),
      CommentatorAction::Recover
    );
  }

  #[test]
  fn should_restart_when_the_compaction_never_takes() {
    let mut watch = watch_compacting();
    for _ in 1..COMPACT_SETTLE_POLLS {
      assert_eq!(
        watch.poll(commentator_over_the_line(), never_asked),
        CommentatorAction::Wait
      );
    }

    assert_eq!(
      watch.poll(commentator_over_the_line(), never_asked),
      CommentatorAction::Restart
    );
  }

  #[test]
  fn should_try_a_restart_again_when_the_commentator_was_not_replaced() {
    let mut watch = watch_compacting();
    for _ in 0..COMPACT_SETTLE_POLLS {
      watch.poll(commentator_over_the_line(), never_asked);
    }
    for _ in 1..COMPACT_SETTLE_POLLS {
      assert_eq!(
        watch.poll(commentator_at(20_000), never_asked),
        CommentatorAction::Wait
      );
    }

    assert_eq!(
      watch.poll(commentator_over_the_line(), never_asked),
      CommentatorAction::Restart
    );
  }

  #[test]
  fn should_leave_an_unmeasured_commentator_at_work_until_its_turnover() {
    let mut watch = CommentatorWatch::new(false, ContextSize::UNKNOWN);

    assert_eq!(
      watch.poll(
        unmeasured_commentator(TURNOVER_REVIEWS - 1, TURNOVER_SECONDS - 1),
        never_asked
      ),
      CommentatorAction::Wait
    );
  }

  #[test]
  fn should_restart_an_unmeasured_commentator_after_enough_reviews() {
    let mut watch = CommentatorWatch::new(false, ContextSize::UNKNOWN);

    assert_eq!(
      watch.poll(unmeasured_commentator(TURNOVER_REVIEWS, 60), never_asked),
      CommentatorAction::AskToStop
    );
    assert_eq!(
      watch.poll(unmeasured_commentator(TURNOVER_REVIEWS, 65), idle),
      CommentatorAction::Restart
    );
  }

  #[test]
  fn should_restart_an_unmeasured_commentator_after_long_enough() {
    let mut watch = CommentatorWatch::new(false, ContextSize::UNKNOWN);

    assert_eq!(
      watch.poll(unmeasured_commentator(0, TURNOVER_SECONDS), never_asked),
      CommentatorAction::AskToStop
    );
    assert_eq!(
      watch.poll(unmeasured_commentator(0, TURNOVER_SECONDS + 5), idle),
      CommentatorAction::Restart
    );
  }

  #[test]
  fn should_never_compact_a_commentator_that_cannot_compact_itself() {
    let mut watch = CommentatorWatch::new(false, ContextSize::tokens(138_000));

    for _ in 0..OVER_LINE_POLLS {
      assert_eq!(
        watch.poll(commentator_over_the_line(), never_asked),
        CommentatorAction::Wait
      );
    }
    assert_eq!(
      watch.poll(commentator_at(20_000), never_asked),
      CommentatorAction::Wait
    );
  }
}

mod holds_commits {
  use super::*;

  #[test]
  fn should_work() {
    let mut watch = watch_asked_to_stop();
    assert!(watch.holds_commits());

    watch.poll(commentator_over_the_line(), idle);
    assert!(watch.holds_commits());

    watch.poll(commentator_at(20_000), never_asked);
    assert!(!watch.holds_commits());
  }

  #[test]
  fn should_not_hold_commits_from_a_commentator_at_work() {
    let mut watch = self_compacting_watch();
    watch.poll(commentator_over_the_line(), never_asked);

    assert!(!watch.holds_commits());
  }

  #[test]
  fn should_hold_commits_until_a_commentator_owed_a_restart_is_replaced() {
    let mut watch = watch_compacting();
    for _ in 0..COMPACT_SETTLE_POLLS {
      watch.poll(commentator_over_the_line(), never_asked);
    }

    watch.poll(commentator_at(20_000), never_asked);
    assert!(watch.holds_commits());
  }
}

mod owe_recovery {
  use super::*;

  #[test]
  fn should_work() {
    let mut watch = self_compacting_watch();
    watch.poll(commentator_at(138_000), never_asked);
    assert_eq!(
      watch.poll(commentator_at(20_000), never_asked),
      CommentatorAction::Recover
    );

    watch.owe_recovery();

    assert_eq!(
      watch.poll(commentator_at(21_000), never_asked),
      CommentatorAction::Recover
    );
    assert_eq!(
      watch.poll(commentator_at(22_000), never_asked),
      CommentatorAction::Wait
    );
  }
}
