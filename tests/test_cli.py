import json
import os
import re
import shlex
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


TESTS = Path(__file__).resolve().parent
SKILL = TESTS.parent / "skills" / "chainsaw"


class SupervisorSmokeTest(unittest.TestCase):
    def test_implementer_task_is_dispatched_and_accepted(self):
        override = os.environ.get("CHAINSAW_SUPERVISOR_COMMAND")
        if override:
            supervisor = shlex.split(override)
        else:
            # Let the shipped wrapper build once, before isolating HOME.
            subprocess.run([SKILL / "bin" / "chainsaw", "--help"], cwd=SKILL / "coordinator",
                           stdout=subprocess.DEVNULL, check=True)
            supervisor = [SKILL / "coordinator" / "target" / "debug" / "chainsaw"]

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = root / "run"
            repo.mkdir()
            home = root / "home"
            project = re.sub(r"[^A-Za-z0-9]", "-", str(repo.resolve()))
            transcript = home / ".claude" / "projects" / project / "worker.jsonl"
            transcript.parent.mkdir(parents=True)
            (root / "herdr").symlink_to(TESTS / "fake_herdr.py")

            env = dict(
                # Git hooks can export GIT_DIR and GIT_INDEX_FILE for the caller's repo.
                {key: value for key, value in os.environ.items() if not key.startswith("GIT_")},
                HOME=str(home),
                PATH=str(root) + os.pathsep + os.environ["PATH"],
                HERDR_WORKSPACE_ID="workspace",
                WORKER_TRANSCRIPT=str(transcript),
                CHAINSAW_CONFIG="",
                XDG_CONFIG_HOME=str(home / ".config"),
                GIT_AUTHOR_NAME="Test", GIT_COMMITTER_NAME="Test",
                GIT_AUTHOR_EMAIL="test@example.invalid", GIT_COMMITTER_EMAIL="test@example.invalid",
            )
            supervisor += ["--run-dir", repo]

            def run(command, *, stdin=None):
                result = subprocess.run(command, env=env, input=stdin, text=True,
                                        capture_output=True, timeout=10)
                self.assertEqual(result.returncode, 0, f"{command}\n{result.stdout}{result.stderr}")
                return result.stdout.strip()

            def git(*args):
                return run(["git", "-C", repo, *args])

            def cli(*args, stdin=None):
                return run([*supervisor, *args], stdin=stdin)

            git("init", "-q", "-b", "main")
            git("commit", "--allow-empty", "-qm", "initial commit")
            task = cli("task", "new", "--files", "work.txt", "--predicted-lines", "1",
                       stdin="Create work.txt")
            self.assertEqual(task, "1")
            self.assertEqual(json.loads(cli("launch", "worker", "Ready?"))["name"], "worker")
            self.assertIn(f"task {task} dispatched", cli("dispatch", task, "--to", "worker"))

            (repo / "work.txt").write_text("done\n")
            git("add", "work.txt")
            commit_output = git("commit", "-m", "feat: add work")
            with transcript.open("a") as log:
                block = {"type": "tool_result", "content": commit_output, "tool_use_id": "commit"}
                print(json.dumps({"type": "user", "message": {"content": [block]}}), file=log)

            transcript.with_name("lead.jsonl").touch()
            with subprocess.Popen(
                [*supervisor, "daemon", "--lead", "lead", "--session-id", "lead",
                 "--poll-interval-ms", "25"],
                env=env,
            ) as daemon:
                try:
                    deadline = time.monotonic() + 5
                    state = cli("state", "--task", task)
                    while state != f"{task} committed_unverified":
                        self.assertIsNone(daemon.poll(), "daemon exited while observing the task")
                        self.assertLess(time.monotonic(), deadline, f"last task state: {state}")
                        time.sleep(0.025)
                        state = cli("state", "--task", task)
                    self.assertIn("stopped", cli("stop"))
                    self.assertEqual(daemon.wait(timeout=5), 0)
                finally:
                    if daemon.poll() is None:
                        daemon.kill()

            sha = git("rev-parse", "--short", "HEAD")
            self.assertIn(f"checks passed at {sha}", cli("accept", task))
            self.assertEqual(cli("state", "--task", task), f"{task} accepted")
