use crate::domain::test_helpers::{calibration, calibration_measuring, format_calibration};

mod new {
  use super::*;

  #[test]
  fn should_work() {
    let calibration = calibration(3, 7, Some(12.5)).unwrap();

    assert_eq!(
      format_calibration(&calibration),
      r#"id: 3
task_id: 7
predicted_files: 2
predicted_lines: 20
actual_files: 4
actual_lines: 35
wall_seconds: 12.5
created_at: 2023-11-14T22:13:20Z
context_size_start: 100
context_size_end: 900"#
    );
  }

  #[test]
  fn should_accept_an_absent_wall_time() {
    let calibration = calibration(3, 7, None).unwrap();
    assert_eq!(calibration.wall_seconds(), None);
  }

  #[test]
  fn should_accept_zero_measurements_and_an_unchanged_context() {
    let calibration = calibration_measuring(3, 7, Some(0.0), [0, 0, 0, 0, 500, 500]).unwrap();

    assert_eq!(
      format_calibration(&calibration),
      r#"id: 3
task_id: 7
predicted_files: 0
predicted_lines: 0
actual_files: 0
actual_lines: 0
wall_seconds: 0
created_at: 2023-11-14T22:13:20Z
context_size_start: 500
context_size_end: 500"#
    );
  }

  #[test]
  fn should_fail_when_the_id_is_not_positive() {
    for id in [i64::MIN, -1, 0] {
      let error = calibration(id, 7, None).unwrap_err();
      assert_eq!(error.to_string(), "id must be positive");
    }
  }

  #[test]
  fn should_fail_when_the_task_id_is_not_positive() {
    for task_id in [i64::MIN, -1, 0] {
      let error = calibration(3, task_id, None).unwrap_err();
      assert_eq!(error.to_string(), "task_id must be positive");
    }
  }

  #[test]
  fn should_fail_when_a_measurement_is_negative() {
    let fields = [
      "predicted_files",
      "predicted_lines",
      "actual_files",
      "actual_lines",
      "context_size_start",
      "context_size_end",
    ];
    for (index, field) in fields.into_iter().enumerate() {
      let mut measurements = [0; 6];
      measurements[index] = -1;
      let error = calibration_measuring(3, 7, None, measurements).unwrap_err();
      assert_eq!(error.to_string(), format!("{field} cannot be negative"));
    }
  }

  #[test]
  fn should_fail_when_the_wall_time_is_negative_or_not_finite() {
    for seconds in [-0.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
      let error = calibration(3, 7, Some(seconds)).unwrap_err();
      assert_eq!(
        error.to_string(),
        "wall_seconds must be finite and nonnegative"
      );
    }
  }

  #[test]
  fn should_fail_when_the_context_ends_before_it_starts() {
    let error = calibration_measuring(3, 7, None, [0, 0, 0, 0, 900, 899]).unwrap_err();
    assert_eq!(
      error.to_string(),
      "context_size_end cannot precede context_size_start"
    );
  }
}
