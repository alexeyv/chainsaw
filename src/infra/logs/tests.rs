use std::collections::BTreeMap;

use super::{format_growth, transcript_growth};

fn sizes(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
  pairs
    .iter()
    .map(|(name, size)| ((*name).to_owned(), *size))
    .collect()
}

mod transcript_growth {
  use super::*;

  #[test]
  fn should_work() {
    let before = sizes(&[("a", 10), ("b", 20)]);
    let after = sizes(&[("a", 15), ("b", 20)]);

    assert_eq!(
      format!("{:?}", transcript_growth(&before, &after)),
      r#"[("a", 5)]"#
    );
  }

  #[test]
  fn should_count_a_new_transcript_as_growth_from_zero() {
    let before = sizes(&[("a", 10)]);
    let after = sizes(&[("a", 10), ("b", 7)]);

    assert_eq!(
      format!("{:?}", transcript_growth(&before, &after)),
      r#"[("b", 7)]"#
    );
  }

  #[test]
  fn should_ignore_a_transcript_that_shrank_or_vanished() {
    let before = sizes(&[("a", 10), ("b", 20)]);
    let after = sizes(&[("a", 4)]);

    assert_eq!(format!("{:?}", transcript_growth(&before, &after)), "[]");
  }
}

mod format_growth {
  use super::*;

  #[test]
  fn should_work() {
    let growth = vec![("a".to_owned(), 5), ("b".to_owned(), 2)];

    assert_eq!(
      format_growth(&growth).as_deref(),
      Some("transcripts grew: a +5, b +2")
    );
  }

  #[test]
  fn should_be_silent_when_nothing_grew() {
    assert_eq!(format_growth(&[]), None);
  }
}
