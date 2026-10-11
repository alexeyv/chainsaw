use super::*;
use crate::infra::agent::Claude;
use crate::infra::agent::tests::Transcript;

mod check_readable {
  use super::*;

  #[test]
  fn should_work() {
    let transcript = Transcript::written();

    transcript_file(&Claude, &transcript.path())
      .check_readable()
      .unwrap();
  }

  #[test]
  fn should_fail_when_the_file_is_gone() {
    let transcript = Transcript::unwritten();

    let error = transcript_file(&Claude, &transcript.path())
      .check_readable()
      .unwrap_err();

    assert_eq!(
      error.downcast::<std::io::Error>().unwrap().kind(),
      std::io::ErrorKind::NotFound
    );
  }
}
