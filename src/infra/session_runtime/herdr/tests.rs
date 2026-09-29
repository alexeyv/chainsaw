use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static NEXT_SHIM: AtomicU64 = AtomicU64::new(0);
static SHIM: OnceLock<PathBuf> = OnceLock::new();

/// macOS scans a freshly written executable on its first run, which costs far more
/// than the run itself. Write the shim once per test process and hard-link it into
/// each fixture; the links share the scanned inode, and `$0` still names the link,
/// so every fixture records into its own directory.
fn shim() -> &'static Path {
  SHIM.get_or_init(|| {
    let dir = env::temp_dir().join(format!("chainsaw-herdr-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let program = dir.join("herdr");
    fs::write(&program, FAKE_HERDR).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    // Absorb the one-time scan here rather than inside whichever test runs first.
    let _ = Command::new(&program).arg("warm").output();
    program
  })
}

/// A `herdr` standing in at the real process boundary: it records the argv it was
/// handed and answers with the JSON shapes and exit codes the real CLI produces.
const FAKE_HERDR: &str = r#"#!/bin/sh
calls="$(dirname "$0")/calls"
for argument in "$@"; do printf '%s\n' "$argument" >> "$calls"; done
printf '%s\n' 'END-OF-CALL' >> "$calls"
case "$1 $2" in
'tab create')
  printf '{"result":{"root_pane":{"pane_id":"pane-7"},"tab":{"tab_id":"tab-7"}}}\n' ;;
'pane split')
  printf '{"result":{"pane":{"pane_id":"pane-9"}}}\n' ;;
'agent start')
  case "$3" in
  late-id|no-id) printf '{"result":{"agent":{"status":"starting"}}}\n' ;;
  *) printf '{"result":{"agent":{"agent_session":{"value":"sess-1"},"status":"idle"}}}\n' ;;
  esac ;;
'agent get')
  if [ "$3" = missing ]; then printf 'no such agent\n' >&2; exit 1; fi
  if [ "$3" = malformed ]; then printf 'not json\n'; exit 0; fi
  if [ "$3" = no-id ]; then printf '{"result":{"agent":{"status":"starting"}}}\n'; exit 0; fi
  if [ "$3" = late-id ]; then
    # The name appears once for `tab create --label`, once for `agent start`, and
    # once per `agent get`; report the id on the third get.
    if [ "$(grep -c '^late-id$' "$calls")" -lt 5 ]; then
      printf '{"result":{"agent":{"status":"starting"}}}\n'; exit 0
    fi
    printf '{"result":{"agent":{"agent_session":{"value":"abc"},"status":"idle"}}}\n'; exit 0
  fi
  printf '{"result":{"agent":{"agent_session":{"value":"sess-1"},"status":"working"}}}\n' ;;
'agent prompt')
  printf '{"result":{"delivered":true}}\n' ;;
'agent send-keys')
  printf '{"result":{"delivered":true}}\n' ;;
'agent wait')
  printf '{"result":{"status":"idle"}}\n' ;;
*)
  printf 'unsupported: %s\n' "$*" >&2; exit 2 ;;
esac
"#;

struct FakeHerdr {
  dir: PathBuf,
  program: PathBuf,
  calls: PathBuf,
}

impl FakeHerdr {
  fn new() -> Self {
    let sequence = NEXT_SHIM.fetch_add(1, Ordering::Relaxed);
    let dir = shim().parent().unwrap().join(format!("fixture-{sequence}"));
    fs::create_dir_all(&dir).unwrap();
    let program = dir.join("herdr");
    fs::hard_link(shim(), &program).unwrap();
    let calls = dir.join("calls");
    Self {
      dir,
      program,
      calls,
    }
  }

  fn runtime(&self, workspace: Option<&str>, tab_id: &str) -> HerdrSessionRuntime {
    HerdrSessionRuntime {
      program: OsString::from(&self.program),
      workspace: workspace.map(str::to_owned),
      tab_id: tab_id.to_owned(),
      session_id_poll_interval: Duration::from_millis(1),
      session_id_poll_attempts: 4,
    }
  }

  /// Every invocation's argv, in order.
  fn calls(&self) -> Vec<Vec<String>> {
    let text = fs::read_to_string(&self.calls).unwrap_or_default();
    let mut calls = Vec::new();
    let mut current = Vec::new();
    for line in text.lines() {
      if line == "END-OF-CALL" {
        calls.push(std::mem::take(&mut current));
      } else {
        current.push(line.to_owned());
      }
    }
    calls
  }
}

impl Drop for FakeHerdr {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.dir);
  }
}

mod start {
  use super::*;

  #[test]
  fn should_work() {
    let herdr = FakeHerdr::new();
    let runtime = herdr.runtime(Some("workspace-1"), "ambient-tab");
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
    let herdr = FakeHerdr::new();
    let runtime = herdr.runtime(Some("workspace-1"), "ambient-tab");
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
    let herdr = FakeHerdr::new();
    let runtime = herdr.runtime(Some("workspace-1"), "ambient-tab");
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
    let herdr = FakeHerdr::new();
    let runtime = herdr.runtime(Some("workspace-1"), "ambient-tab");

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
    let herdr = FakeHerdr::new();
    let runtime = herdr.runtime(Some("workspace-1"), "ambient-tab");

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
    let herdr = FakeHerdr::new();
    let runtime = herdr.runtime(Some("workspace-1"), "ambient-tab");

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
    let herdr = FakeHerdr::new();
    let runtime = herdr.runtime(None, "");

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
    let herdr = FakeHerdr::new();

    let status = herdr
      .runtime(Some("workspace-1"), "")
      .status("worker")
      .unwrap();

    assert_eq!(status, Some(SessionStatus::Busy));
    assert_eq!(herdr.calls()[0], ["agent", "get", "worker"]);
  }

  #[test]
  fn should_report_nothing_when_herdr_does_not_know_the_agent() {
    let herdr = FakeHerdr::new();

    let status = herdr
      .runtime(Some("workspace-1"), "")
      .status("missing")
      .unwrap();

    assert_eq!(status, None);
  }

  #[test]
  fn should_report_nothing_when_herdr_answers_with_invalid_json() {
    let herdr = FakeHerdr::new();

    let status = herdr
      .runtime(Some("workspace-1"), "")
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
    let herdr = FakeHerdr::new();

    herdr
      .runtime(Some("workspace-1"), "")
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
    let herdr = FakeHerdr::new();

    herdr
      .runtime(Some("workspace-1"), "")
      .interrupt("worker")
      .unwrap();

    assert_eq!(herdr.calls()[0], ["agent", "send-keys", "worker", "esc"]);
  }
}

mod wait {
  use super::*;

  #[test]
  fn should_work() {
    let herdr = FakeHerdr::new();

    herdr
      .runtime(Some("workspace-1"), "")
      .wait("worker", Duration::from_secs(30))
      .unwrap();

    assert_eq!(
      herdr.calls()[0],
      ["agent", "wait", "worker", "--timeout", "30000"]
    );
  }
}
