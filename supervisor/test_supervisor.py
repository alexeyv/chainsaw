import importlib.util
import io
import os
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "chainsaw_supervisor", os.path.join(os.path.dirname(__file__), "supervisor.py")
)
supervisor = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(supervisor)


class GateLastTests(unittest.TestCase):
    gate = 'make all && make test && make tidy; echo "GATE_EXIT: $?"'

    def test_redirected_gate_is_recognized_and_outside_writes_are_harmless(self):
        with tempfile.TemporaryDirectory() as root:
            run_dir = os.path.join(root, "worktree")
            notes = os.path.join(root, "decisions.md")
            scratch = os.path.join(root, "scratch")
            os.mkdir(run_dir)
            commands = [
                (self.gate, True),
                ("python3 - <<'PY'\nopen('source.py', 'w').write('changed')\nPY", True),
                ('make all && make test && make tidy > /dev/null 2>&1; '
                 'echo "GATE_EXIT: $?"', True),
                (f"F={notes}\ncat >> \"$F\" <<'EOF'\nnotes\nEOF", True),
                (f"SP={scratch}\ncat > \"$SP/msg.txt\" <<'EOF'\nmessage\nEOF", True),
                ("git add source.py \\\n+                    another-source.py", True),
                (f'git commit -F "{scratch}/msg.txt"', True),
            ]

            self.assertIsNone(supervisor.gate_last_problem(commands, self.gate, run_dir))

    def test_opaque_command_run_outside_worktree_is_harmless(self):
        with tempfile.TemporaryDirectory() as root:
            run_dir = os.path.join(root, "worktree")
            notes_dir = os.path.join(root, "notes")
            os.mkdir(run_dir)
            os.mkdir(notes_dir)
            command = (
                f"cd {notes_dir}\npython3 - <<'PY'\n"
                "from pathlib import Path\nPath('decisions.md').write_text('x')\nPY"
            )

            self.assertTrue(supervisor._command_harmless_after_gate(command, run_dir))

    def test_redirect_into_worktree_remains_source_modifying(self):
        with tempfile.TemporaryDirectory() as run_dir:
            command = "cat > source.py <<'EOF'\nchanged\nEOF"

            self.assertFalse(supervisor._command_harmless_after_gate(command, run_dir))

    def test_failed_redirected_gate_remains_a_failure(self):
        with tempfile.TemporaryDirectory() as run_dir:
            commands = [
                ('make all && make test && make tidy > /dev/null 2>&1; '
                 'echo "GATE_EXIT: $?"', False),
                ("git commit -m test", True),
            ]

            self.assertEqual(
                supervisor.gate_last_problem(commands, self.gate, run_dir),
                "the gate before the commit failed (error result in the log)",
            )


class SessionLogTests(unittest.TestCase):
    def test_session_log_is_found_outside_run_project_directory(self):
        with tempfile.TemporaryDirectory() as home, mock.patch.dict(
                os.environ, {"HOME": home}):
            run_dir = os.path.join(home, "src", "project", "worktree")
            os.makedirs(run_dir)
            store = supervisor.Store(run_dir)
            sid = "lead-session-id"
            store.db.execute(
                "insert into sessions(name,role,session_id,started_at,last_growth) "
                "values(?,?,?,?,?)",
                ("lead", "lead", sid, 1, 1),
            )
            other_project = os.path.join(home, ".claude", "projects", "-parent")
            os.makedirs(other_project)
            log = os.path.join(other_project, sid + ".jsonl")
            with open(log, "w") as f:
                f.write("{}\n")

            with mock.patch.object(supervisor, "herdr_session_id", return_value=sid):
                self.assertEqual(store.session_log("lead"), log)
                self.assertEqual(
                    store.one("select log_path from sessions where name='lead'")["log_path"],
                    log,
                )

    def test_missing_session_log_is_not_reported_as_zero_context(self):
        store = mock.Mock()
        store.q.return_value = [{"name": "lead"}]
        store.session_log.return_value = None
        output = io.StringIO()

        with redirect_stdout(output):
            supervisor.cmd_context(store)

        self.assertEqual(output.getvalue(),
                         "lead\tUNAVAILABLE (session log not found)\n")


class VerifyRetryTests(unittest.TestCase):
    def test_clean_advanced_head_retries_commit_marker_once(self):
        task = {
            "implementer": "implementer-1",
            "log_offset": 10,
            "base_head": "base",
        }
        store = mock.Mock()
        store.session_log.side_effect = ["/first/log", "/second/log"]
        with mock.patch.object(supervisor, "commits_in_log",
                               side_effect=[[], ["abc1234"]]), \
             mock.patch.object(supervisor, "new_commit_for",
                               side_effect=[None, "abc1234"]), \
             mock.patch.object(supervisor, "_head_advanced_cleanly", return_value=True), \
             mock.patch.object(supervisor.time, "sleep") as sleep:
            sha, log = supervisor._commit_for_verify(store, task)

        self.assertEqual((sha, log), ("abc1234", "/second/log"))
        sleep.assert_called_once_with(supervisor.VERIFY_LOG_RETRY_SECONDS)


if __name__ == "__main__":
    unittest.main()
