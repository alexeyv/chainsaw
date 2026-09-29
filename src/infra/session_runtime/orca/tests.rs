use std::env;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::domain::AgentKind;

static NEXT_SHIM: AtomicU64 = AtomicU64::new(0);
static SHIM: OnceLock<PathBuf> = OnceLock::new();

/// Written once per test process and hard-linked into each fixture, for the
/// reason the Herdr shim is: macOS scans a fresh executable on its first run.
fn shim() -> &'static Path {
  SHIM.get_or_init(|| {
    let dir = env::temp_dir().join(format!("chainsaw-orca-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let program = dir.join("orca");
    fs::write(&program, FAKE_ORCA).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let _ = Command::new(&program).arg("warm").output();
    program
  })
}

/// An `orca` standing in at the real process boundary: it records the argv it
/// was handed and answers with the JSON shapes and exit codes the real CLI
/// produces. The terminal handle after `--terminal` and the title after
/// `--title` pick the scenario.
const FAKE_ORCA: &str = r#"#!/bin/sh
calls="$(dirname "$0")/calls"
for argument in "$@"; do printf '%s\n' "$argument" >> "$calls"; done
printf '%s\n' 'END-OF-CALL' >> "$calls"
handle=""; title=""; previous=""
for argument in "$@"; do
  [ "$previous" = "--terminal" ] && handle="$argument"
  [ "$previous" = "--title" ] && title="$argument"
  previous="$argument"
done
case "$1 $2" in
'terminal create')
  case "$title" in
  refused) printf '{"ok":false,"error":{"code":"worktree_not_found","message":"no such worktree"}}\n'; exit 1 ;;
  garbled) printf 'not json\n' ;;
  crashed) printf 'orca: boom\n' >&2; exit 1 ;;
  *) printf '{"ok":true,"result":{"terminal":{"handle":"term-7","tabId":"tab-7","title":"%s"}}}\n' "$title" ;;
  esac ;;
'terminal split')
  printf '{"ok":true,"result":{"split":{"handle":"term-9","tabId":"tab-9"}}}\n' ;;
'terminal wait')
  case "$handle" in
  term-busy) printf '{"ok":false,"error":{"code":"timeout","message":"timeout"}}\n'; exit 1 ;;
  term-unsettled) printf '{"ok":true,"result":{"wait":{"satisfied":false}}}\n' ;;
  term-gone) printf '{"ok":false,"error":{"code":"terminal_handle_stale","message":"stale handle"}}\n'; exit 1 ;;
  *) printf '{"ok":true,"result":{"wait":{"satisfied":true}}}\n' ;;
  esac ;;
'terminal send')
  printf '{"ok":true,"result":{"send":{"accepted":true}}}\n' ;;
*)
  printf 'unsupported: %s\n' "$*" >&2; exit 2 ;;
esac
"#;

struct FakeOrca {
  dir: PathBuf,
  program: PathBuf,
  calls: PathBuf,
}

impl FakeOrca {
  fn new() -> Self {
    let sequence = NEXT_SHIM.fetch_add(1, Ordering::Relaxed);
    let dir = shim().parent().unwrap().join(format!("fixture-{sequence}"));
    fs::create_dir_all(&dir).unwrap();
    let program = dir.join("orca");
    fs::hard_link(shim(), &program).unwrap();
    let calls = dir.join("calls");
    Self {
      dir,
      program,
      calls,
    }
  }

  fn runtime(&self) -> OrcaSessionRuntime {
    OrcaSessionRuntime {
      cli: Cli::new("orca", &self.program),
      terminal: "ambient-terminal".to_owned(),
      registry: self.dir.join(REGISTRY_FILE_NAME),
      idle_probe: Duration::from_millis(1),
      turn_start_grace: Duration::from_millis(5),
      turn_end_settle: Duration::from_millis(1),
      session_id_poll_interval: Duration::from_millis(1),
      session_id_poll_attempts: 3,
    }
  }

  /// A session already started, as `start` would have remembered it.
  fn registered(&self, name: &str, handle: &str) -> OrcaSessionRuntime {
    let runtime = self.runtime();
    runtime
      .remember(
        name,
        Terminal {
          handle: handle.to_owned(),
          tab_id: "tab-1".to_owned(),
          external_id: "sess-1".to_owned(),
        },
      )
      .unwrap();
    runtime
  }

  /// A run directory that exists and that no agent has ever written a
  /// transcript for.
  fn run_dir(&self) -> PathBuf {
    let run_dir = self.dir.join("run");
    fs::create_dir_all(&run_dir).unwrap();
    run_dir
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

impl Drop for FakeOrca {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.dir);
  }
}

fn is_version_4_uuid(text: &str) -> bool {
  let hyphens: Vec<usize> = text.match_indices('-').map(|(index, _)| index).collect();
  text.len() == 36
    && hyphens == [8, 13, 18, 23]
    && text[14..15] == *"4"
    && text
      .chars()
      .all(|character| character == '-' || character.is_ascii_hexdigit())
}

mod start {
  use super::*;

  #[test]
  fn should_work() {
    let orca = FakeOrca::new();
    let runtime = orca.runtime();
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

    assert!(
      is_version_4_uuid(&started.external_id),
      "{}",
      started.external_id
    );
    assert_eq!(started.pane_id, "term-7");
    assert_eq!(started.tab_id, "tab-7");
    assert_eq!(
      orca.calls(),
      [[
        "terminal",
        "create",
        "--worktree",
        "path:/tmp/run",
        "--title",
        "worker",
        "--command",
        &format!(
          "cd /tmp/run && exec claude --session-id {} --model sonnet --effort medium",
          started.external_id
        ),
        "--json"
      ]]
    );
    let terminal = runtime.terminal_of("worker").unwrap();
    assert_eq!(
      (terminal.handle, terminal.tab_id, terminal.external_id),
      ("term-7".to_owned(), "tab-7".to_owned(), started.external_id)
    );
  }

  #[test]
  fn should_split_the_supervisors_terminal_for_a_commentator() {
    let orca = FakeOrca::new();
    let runtime = orca.runtime();

    let started = runtime
      .start(StartSession {
        id: "commentator",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Commentator,
        agent: AgentKind::Claude,
        args: &[],
      })
      .unwrap();

    assert_eq!(started.pane_id, "term-9");
    assert_eq!(started.tab_id, "tab-9");
    assert_eq!(
      orca.calls(),
      [[
        "terminal",
        "split",
        "--terminal",
        "ambient-terminal",
        "--direction",
        "horizontal",
        "--command",
        &format!(
          "cd /tmp/run && exec claude --session-id {}",
          started.external_id
        ),
        "--json"
      ]]
    );
  }

  #[test]
  fn should_quote_the_command_for_the_shell() {
    let orca = FakeOrca::new();
    let args = ["--append-system-prompt", "be terse"].map(str::to_owned);

    let started = orca
      .runtime()
      .start(StartSession {
        id: "worker",
        run_dir: Path::new("/tmp/my run"),
        kind: SessionKind::Implementer,
        agent: AgentKind::Claude,
        args: &args,
      })
      .unwrap();

    assert_eq!(
      orca.calls()[0][7],
      format!(
        "cd '/tmp/my run' && exec claude --session-id {} --append-system-prompt 'be terse'",
        started.external_id
      )
    );
  }

  #[test]
  fn should_fail_when_an_agent_naming_its_own_sessions_never_writes_a_transcript() {
    let orca = FakeOrca::new();
    let runtime = orca.runtime();
    let run_dir = orca.run_dir();

    let error = runtime
      .start(StartSession {
        id: "worker",
        run_dir: &run_dir,
        kind: SessionKind::Implementer,
        agent: AgentKind::Codex,
        args: &["--dangerously-bypass-approvals-and-sandbox".to_owned()],
      })
      .unwrap_err();

    assert_eq!(
      error.to_string(),
      format!(
        "orca: codex in term-7 never wrote a transcript for {}",
        run_dir.display()
      )
    );
    assert_eq!(
      orca.calls()[0][7],
      format!(
        "cd {} && exec codex --dangerously-bypass-approvals-and-sandbox",
        run_dir.display()
      )
    );
    assert!(runtime.terminal_of("worker").is_err());
  }

  #[test]
  fn should_run_cursor_through_its_cli() {
    let orca = FakeOrca::new();
    let runtime = orca.runtime();
    let run_dir = orca.run_dir();

    let error = runtime
      .start(StartSession {
        id: "worker",
        run_dir: &run_dir,
        kind: SessionKind::Implementer,
        agent: AgentKind::Cursor,
        args: &[
          "--trust".to_owned(),
          "--force".to_owned(),
          "Reply only with the word ready, then wait for the task.".to_owned(),
        ],
      })
      .unwrap_err();

    assert_eq!(
      error.to_string(),
      format!(
        "orca: cursor in term-7 never wrote a transcript for {}",
        run_dir.display()
      )
    );
    assert_eq!(
      orca.calls()[0][7],
      format!(
        "cd {} && exec cursor-agent --trust --force 'Reply only with the word ready, then wait for the task.'",
        run_dir.display()
      )
    );
  }

  #[test]
  fn should_fail_when_orca_refuses() {
    let orca = FakeOrca::new();

    let error = orca
      .runtime()
      .start(StartSession {
        id: "refused",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Implementer,
        agent: AgentKind::Claude,
        args: &[],
      })
      .unwrap_err();

    assert_eq!(
      error.to_string(),
      "orca refused: no such worktree (worktree_not_found)"
    );
  }

  #[test]
  fn should_fail_when_orca_answers_with_invalid_json() {
    let orca = FakeOrca::new();

    let error = orca
      .runtime()
      .start(StartSession {
        id: "garbled",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Implementer,
        agent: AgentKind::Claude,
        args: &[],
      })
      .unwrap_err();

    assert_eq!(error.to_string(), "orca returned invalid JSON");
  }

  #[test]
  fn should_fail_when_orca_fails_without_a_reply() {
    let orca = FakeOrca::new();

    let error = orca
      .runtime()
      .start(StartSession {
        id: "crashed",
        run_dir: Path::new("/tmp/run"),
        kind: SessionKind::Implementer,
        agent: AgentKind::Claude,
        args: &[],
      })
      .unwrap_err();

    assert_eq!(error.to_string(), "orca failed: orca: boom");
  }
}

mod status {
  use super::*;

  #[test]
  fn should_work() {
    let orca = FakeOrca::new();

    let status = orca
      .registered("worker", "term-7")
      .status("worker")
      .unwrap();

    assert_eq!(status, Some(SessionStatus::Idle));
    assert_eq!(
      orca.calls(),
      [[
        "terminal",
        "wait",
        "--terminal",
        "term-7",
        "--for",
        "tui-idle",
        "--timeout-ms",
        "1",
        "--json"
      ]]
    );
  }

  #[test]
  fn should_report_busy_when_the_agent_is_mid_turn() {
    let orca = FakeOrca::new();

    let status = orca
      .registered("worker", "term-busy")
      .status("worker")
      .unwrap();

    assert_eq!(status, Some(SessionStatus::Busy));
  }

  #[test]
  fn should_report_busy_when_the_wait_ends_unsatisfied() {
    let orca = FakeOrca::new();

    let status = orca
      .registered("worker", "term-unsettled")
      .status("worker")
      .unwrap();

    assert_eq!(status, Some(SessionStatus::Busy));
  }

  #[test]
  fn should_report_nothing_for_a_session_never_started() {
    let orca = FakeOrca::new();

    let status = orca.runtime().status("worker").unwrap();

    assert_eq!(status, None);
    assert!(orca.calls().is_empty());
  }

  #[test]
  fn should_report_nothing_when_orca_no_longer_has_the_terminal() {
    let orca = FakeOrca::new();

    let status = orca
      .registered("worker", "term-gone")
      .status("worker")
      .unwrap();

    assert_eq!(status, None);
  }
}

mod prompt {
  use super::*;

  #[test]
  fn should_work() {
    let orca = FakeOrca::new();

    orca
      .registered("worker", "term-7")
      .prompt("worker", "do the thing")
      .unwrap();

    assert_eq!(
      orca.calls(),
      [[
        "terminal",
        "send",
        "--terminal",
        "term-7",
        "--text",
        "do the thing",
        "--enter",
        "--json"
      ]]
    );
  }

  #[test]
  fn should_fail_for_a_session_never_started() {
    let orca = FakeOrca::new();

    let error = orca.runtime().prompt("worker", "hello").unwrap_err();

    assert_eq!(error.to_string(), "orca: no session named worker");
    assert!(orca.calls().is_empty());
  }
}

mod interrupt {
  use super::*;

  #[test]
  fn should_work() {
    let orca = FakeOrca::new();

    orca
      .registered("worker", "term-7")
      .interrupt("worker")
      .unwrap();

    assert_eq!(
      orca.calls(),
      [[
        "terminal",
        "send",
        "--terminal",
        "term-7",
        "--interrupt",
        "--json"
      ]]
    );
  }
}

mod wait {
  use super::*;

  #[test]
  fn should_work() {
    let orca = FakeOrca::new();

    orca
      .registered("worker", "term-busy")
      .wait("worker", Duration::from_secs(30))
      .unwrap();

    assert_eq!(
      orca.calls(),
      [
        [
          "terminal",
          "wait",
          "--terminal",
          "term-busy",
          "--for",
          "tui-idle",
          "--timeout-ms",
          "1",
          "--json"
        ],
        [
          "terminal",
          "wait",
          "--terminal",
          "term-busy",
          "--for",
          "tui-idle",
          "--timeout-ms",
          "30000",
          "--json"
        ]
      ]
    );
  }

  #[test]
  fn should_return_after_the_grace_period_when_the_turn_never_shows() {
    let orca = FakeOrca::new();

    orca
      .registered("worker", "term-7")
      .wait("worker", Duration::from_secs(30))
      .unwrap();

    let calls = orca.calls();
    assert!(!calls.is_empty());
    assert!(
      calls.iter().all(|call| call[7] == "1"),
      "only probes were expected: {calls:?}"
    );
  }

  #[test]
  fn should_return_at_once_when_orca_no_longer_has_the_terminal() {
    let orca = FakeOrca::new();

    orca
      .registered("worker", "term-gone")
      .wait("worker", Duration::from_secs(30))
      .unwrap();

    assert_eq!(orca.calls().len(), 1);
  }

  #[test]
  fn should_fail_for_a_session_never_started() {
    let orca = FakeOrca::new();

    let error = orca
      .runtime()
      .wait("worker", Duration::from_secs(30))
      .unwrap_err();

    assert_eq!(error.to_string(), "orca: no session named worker");
  }
}

mod mint_session_id {
  use super::*;

  #[test]
  fn should_work() {
    let id = mint_session_id().unwrap();

    assert!(is_version_4_uuid(&id), "{id}");
  }

  #[test]
  fn should_differ_between_calls() {
    assert_ne!(mint_session_id().unwrap(), mint_session_id().unwrap());
  }
}
