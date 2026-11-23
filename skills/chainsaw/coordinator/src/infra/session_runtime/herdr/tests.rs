use super::*;
use crate::infra::session_runtime::tests::FakeCli;
use std::path::Path;

impl FakeCli {
  fn herdr_runtime(&self, workspace: Option<&str>, tab_id: &str) -> HerdrSessionRuntime {
    HerdrSessionRuntime {
      cli: Cli::new("herdr", &self.program),
      workspace: workspace.map(str::to_owned),
      tab_id: tab_id.to_owned(),
      session_id_poll_interval: Duration::from_millis(1),
      session_id_poll_attempts: 4,
    }
  }
}

mod start {
  use super::*;

  #[test]
  fn should_work() {
    let herdr = FakeCli::new("herdr");
    let runtime = herdr.herdr_runtime(Some("workspace-1"), "ambient-tab");
    let args = ["--model", "sonnet", "--effort", "medium"].map(str::to_owned);

    let started = runtime
      .start(StartSession {
        id: "worker",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Implementer,
        agent: AgentKind::Claude,
        args: &args,
      })
      .unwrap();

    assert_eq!(started.external_id, "sess-1");
    assert_eq!(started.pane_id, "pane-7");
    assert_eq!(started.tab_id, "tab-7");
    let calls = herdr.calls();
    assert_eq!(
      calls[0],
      [
        "tab",
        "create",
        "--workspace",
        "workspace-1",
        "--label",
        "worker",
        "--cwd",
        "/tmp/run",
        "--no-focus"
      ]
    );
    assert_eq!(
      calls[1][..8],
      [
        "agent", "start", "worker", "--kind", "claude", "--pane", "pane-7", "--"
      ]
    );
    assert_eq!(calls[1][8..], args);
  }

  #[test]
  fn should_start_codex_under_its_own_kind() {
    let herdr = FakeCli::new("herdr");
    let runtime = herdr.herdr_runtime(Some("workspace-1"), "ambient-tab");
    let args = ["--dangerously-bypass-approvals-and-sandbox", "."].map(str::to_owned);

    let started = runtime
      .start(StartSession {
        id: "worker",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Implementer,
        agent: AgentKind::Codex,
        args: &args,
      })
      .unwrap();

    assert_eq!(started.external_id, "sess-1");
    let calls = herdr.calls();
    assert_eq!(
      calls[1][..8],
      [
        "agent", "start", "worker", "--kind", "codex", "--pane", "pane-7", "--"
      ]
    );
    assert_eq!(calls[1][8..], args);
    assert_eq!(calls.len(), 2, "{calls:?}");
  }

  #[test]
  fn should_start_cursor_under_its_own_kind() {
    let herdr = FakeCli::new("herdr");
    let runtime = herdr.herdr_runtime(Some("workspace-1"), "ambient-tab");
    let args = [
      "--trust",
      "--force",
      "Reply only with the word ready, then wait for the task.",
    ]
    .map(str::to_owned);

    let started = runtime
      .start(StartSession {
        id: "worker",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Implementer,
        agent: AgentKind::Cursor,
        args: &args,
      })
      .unwrap();

    assert_eq!(started.external_id, "sess-1");
    let calls = herdr.calls();
    assert_eq!(
      calls[1][..8],
      [
        "agent", "start", "worker", "--kind", "cursor", "--pane", "pane-7", "--"
      ]
    );
    assert_eq!(calls[1][8..], args);
    assert_eq!(calls.len(), 2, "{calls:?}");
  }

  #[test]
  fn should_split_the_current_pane_and_keep_the_ambient_tab_for_a_commentator() {
    let herdr = FakeCli::new("herdr");
    let runtime = herdr.herdr_runtime(Some("workspace-1"), "ambient-tab");

    let started = runtime
      .start(StartSession {
        id: "commentator",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Commentator,
        agent: AgentKind::Claude,
        args: &[],
      })
      .unwrap();

    assert_eq!(started.pane_id, "pane-9");
    assert_eq!(started.tab_id, "ambient-tab");
    assert_eq!(
      herdr.calls()[0],
      [
        "pane",
        "split",
        "--current",
        "--direction",
        "right",
        "--cwd",
        "/tmp/run",
        "--no-focus"
      ]
    );
  }

  #[test]
  fn should_poll_agent_get_when_agent_start_reports_no_session_id() {
    let herdr = FakeCli::new("herdr");
    let runtime = herdr.herdr_runtime(Some("workspace-1"), "ambient-tab");

    let started = runtime
      .start(StartSession {
        id: "late-id",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Implementer,
        agent: AgentKind::Claude,
        args: &[],
      })
      .unwrap();

    assert_eq!(started.external_id, "abc");
    let calls = herdr.calls();
    assert_eq!(calls[1][..3], ["agent", "start", "late-id"]);
    assert_eq!(
      calls[2..],
      [
        ["agent", "get", "late-id"],
        ["agent", "get", "late-id"],
        ["agent", "get", "late-id"]
      ]
    );
  }

  #[test]
  fn should_fail_when_agent_get_never_reports_a_session_id() {
    let herdr = FakeCli::new("herdr");
    let runtime = herdr.herdr_runtime(Some("workspace-1"), "ambient-tab");

    let error = runtime
      .start(StartSession {
        id: "no-id",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Implementer,
        agent: AgentKind::Claude,
        args: &[],
      })
      .unwrap_err();

    assert_eq!(error.to_string(), "herdr agent never reported a session id");
    let gets = herdr
      .calls()
      .iter()
      .filter(|call| call[..2] == ["agent", "get"])
      .count();
    assert_eq!(gets, runtime.session_id_poll_attempts);
  }

  #[test]
  fn should_fail_when_the_supervisor_is_not_inside_a_herdr_pane() {
    let herdr = FakeCli::new("herdr");
    let runtime = herdr.herdr_runtime(None, "");

    let error = runtime
      .start(StartSession {
        id: "worker",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Implementer,
        agent: AgentKind::Claude,
        args: &[],
      })
      .unwrap_err();

    assert_eq!(
      error.to_string(),
      "supervisor: must run inside a Herdr pane"
    );
    assert!(herdr.calls().is_empty());
  }
}

mod status {
  use super::*;

  #[test]
  fn should_work() {
    let herdr = FakeCli::new("herdr");

    let status = herdr
      .herdr_runtime(Some("workspace-1"), "")
      .status("worker")
      .unwrap();

    assert_eq!(status, Some(SessionStatus::Busy));
    assert_eq!(herdr.calls()[0], ["agent", "get", "worker"]);
  }

  #[test]
  fn should_report_nothing_when_herdr_does_not_know_the_agent() {
    let herdr = FakeCli::new("herdr");

    let status = herdr
      .herdr_runtime(Some("workspace-1"), "")
      .status("missing")
      .unwrap();

    assert_eq!(status, None);
  }

  #[test]
  fn should_report_nothing_when_herdr_answers_with_invalid_json() {
    let herdr = FakeCli::new("herdr");

    let status = herdr
      .herdr_runtime(Some("workspace-1"), "")
      .status("malformed")
      .unwrap();

    assert_eq!(status, None);
  }
}

mod status_named {
  use super::*;

  #[test]
  fn should_work() {
    assert_eq!(status_named("working"), SessionStatus::Busy);
  }

  #[test]
  fn should_report_idle_when_the_agent_is_at_its_prompt() {
    assert_eq!(status_named("idle"), SessionStatus::Idle);
  }

  #[test]
  fn should_report_idle_when_the_agent_is_done() {
    assert_eq!(status_named("done"), SessionStatus::Idle);
  }

  #[test]
  fn should_report_busy_when_the_agent_waits_on_a_permission() {
    assert_eq!(status_named("blocked"), SessionStatus::Busy);
  }

  #[test]
  fn should_not_guess_when_herdr_cannot_tell() {
    assert_eq!(status_named("unknown"), SessionStatus::Unknown);
  }
}

mod prompt {
  use super::*;

  #[test]
  fn should_work() {
    let herdr = FakeCli::new("herdr");

    herdr
      .herdr_runtime(Some("workspace-1"), "")
      .prompt("worker", "do the thing")
      .unwrap();

    assert_eq!(
      herdr.calls()[0],
      ["agent", "prompt", "worker", "do the thing"]
    );
  }
}

mod interrupt {
  use super::*;

  #[test]
  fn should_work() {
    let herdr = FakeCli::new("herdr");

    herdr
      .herdr_runtime(Some("workspace-1"), "")
      .interrupt("worker")
      .unwrap();

    assert_eq!(herdr.calls()[0], ["agent", "send-keys", "worker", "esc"]);
  }
}

mod wait {
  use super::*;

  #[test]
  fn should_work() {
    let herdr = FakeCli::new("herdr");

    herdr
      .herdr_runtime(Some("workspace-1"), "")
      .wait("worker", Duration::from_secs(30))
      .unwrap();

    assert_eq!(
      herdr.calls()[0],
      ["agent", "wait", "worker", "--timeout", "30000"]
    );
  }
}

mod abandon {
  use super::*;

  #[test]
  fn should_work() {
    let herdr = FakeCli::new("herdr");
    let runtime = herdr.herdr_runtime(Some("ws-1"), "tab-1");

    runtime.abandon("worker", "pane-7").unwrap();

    assert_eq!(herdr.calls(), [["pane", "close", "pane-7"]]);
  }

  #[test]
  fn should_fail_when_herdr_cannot_close_the_pane() {
    let herdr = FakeCli::new("herdr");
    let runtime = herdr.herdr_runtime(Some("ws-1"), "tab-1");

    let error = runtime.abandon("worker", "pane-stuck").unwrap_err();

    assert_eq!(error.to_string(), "herdr failed: no such pane");
  }
}
