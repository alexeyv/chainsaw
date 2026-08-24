"""Fixtures for exercising the supervisor only through its process boundary."""

import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[1]
SUPERVISOR = PROJECT_ROOT / "supervisor" / "supervisor.py"
FAKE_HERDR = Path(__file__).parent / "fixtures" / "fake_herdr.py"


class SupervisorContractCase(unittest.TestCase):
    """An isolated installation, Git repository, and Herdr service per test."""

    maxDiff = None

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="chainsaw-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.sandbox = Path(self.temporary.name)
        self.run_dir = self.sandbox / "run"
        self.home = self.sandbox / "home"
        self.bin_dir = self.sandbox / "bin"
        self.fake_state = self.sandbox / "fake-herdr.json"
        self.run_dir.mkdir()
        self.home.mkdir()
        self.bin_dir.mkdir()

        fake = self.bin_dir / "herdr"
        shutil.copy2(FAKE_HERDR, fake)
        fake.chmod(0o755)

        self.env = os.environ.copy()
        self.env.update({
            "HOME": str(self.home),
            "PATH": str(self.bin_dir) + os.pathsep + self.env.get("PATH", ""),
            "FAKE_HERDR_STATE": str(self.fake_state),
            "HERDR_WORKSPACE_ID": "contract-workspace",
            "HERDR_TAB_ID": "contract-tab",
            "GIT_AUTHOR_NAME": "Chainsaw Tests",
            "GIT_AUTHOR_EMAIL": "chainsaw-tests@example.invalid",
            "GIT_COMMITTER_NAME": "Chainsaw Tests",
            "GIT_COMMITTER_EMAIL": "chainsaw-tests@example.invalid",
        })
        configured = os.environ.get("CHAINSAW_SUPERVISOR_COMMAND")
        self.supervisor_command = (
            shlex.split(configured) if configured
            else [sys.executable, str(SUPERVISOR)]
        )

        self.git("init", "-q", "-b", "master")
        (self.run_dir / "seed.txt").write_text("initial\n")
        self.git("add", "seed.txt")
        self.git("commit", "-q", "-m", "chore: initial fixture")
        self._tool_use_sequence = 0

    @property
    def logs_dir(self):
        munged = os.path.realpath(self.run_dir).replace("/", "-").replace(".", "-")
        return self.home / ".claude" / "projects" / munged

    def cli(self, *args, input_text=None, timeout=30):
        command = [*self.supervisor_command, "--run-dir", str(self.run_dir), *map(str, args)]
        return subprocess.run(
            command,
            input=input_text,
            text=True,
            capture_output=True,
            env=self.env,
            timeout=timeout,
        )

    def assert_success(self, result):
        self.assertEqual(
            result.returncode,
            0,
            f"command failed\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}",
        )
        return result

    def assert_failure(self, result, message=None):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        if message:
            self.assertIn(message, result.stdout + result.stderr)
        return result

    def git(self, *args, check=True):
        return subprocess.run(
            ["git", "-C", str(self.run_dir), *map(str, args)],
            text=True,
            capture_output=True,
            env=self.env,
            check=check,
        )

    def head(self):
        return self.git("rev-parse", "HEAD").stdout.strip()

    def commit_file(self, path="work.txt", content="changed\n", message="feat: fixture change"):
        target = self.run_dir / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)
        self.git("add", path)
        self.git("commit", "-q", "-m", message)
        return self.head()

    def commit_with_trailer(self):
        (self.run_dir / "trailer.txt").write_text("trailer\n")
        self.git("add", "trailer.txt")
        self.git(
            "commit", "-q", "-m", "feat: attributed fixture", "-m",
            "Co-Authored-By: Fixture <fixture@example.invalid>",
        )
        return self.head()

    def new_task(self, text="Implement the fixture task.", files="work.txt", lines=10):
        args = ["task", "new", "--predicted-lines", str(lines)]
        if isinstance(files, int):
            args.extend(["--predicted-files", str(files)])
        else:
            args.extend(["--files", files])
        result = self.assert_success(self.cli(*args, input_text=text))
        return int(result.stdout.strip())

    def launch(self, name="worker"):
        return self.assert_success(self.cli("launch", name))

    def dispatch(self, task_id, name="worker", reuse=False):
        args = ["dispatch", str(task_id), "--to", name]
        if reuse:
            args.append("--reuse")
        return self.cli(*args)

    def fake_herdr_state(self):
        return json.loads(self.fake_state.read_text())

    def update_fake_herdr(self, **updates):
        state = self.fake_herdr_state() if self.fake_state.exists() else {
            "agents": {}, "panes": {}, "sequence": 0, "drop_prompts": 0,
        }
        state.update(updates)
        self.fake_state.write_text(json.dumps(state, sort_keys=True))

    def session_log(self, name):
        state = self.fake_herdr_state()
        session_id = state["agents"][name]["session_id"]
        return self.logs_dir / f"{session_id}.jsonl"

    def append_log(self, name, entry):
        path = self.session_log(name)
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a") as stream:
            stream.write(json.dumps(entry, separators=(",", ":")) + "\n")

    def append_text(self, name, text):
        self.append_log(name, {
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": text}]},
        })

    def append_usage(self, name, input_tokens=0, cache_read=0, cache_creation=0,
                     sidechain=False):
        self.append_log(name, {
            "type": "assistant",
            "isSidechain": sidechain,
            "message": {
                "content": [],
                "usage": {
                    "input_tokens": input_tokens,
                    "cache_read_input_tokens": cache_read,
                    "cache_creation_input_tokens": cache_creation,
                },
            },
        })

    def append_bash(self, name, command, ok=True):
        self._tool_use_sequence += 1
        tool_id = f"tool-{self._tool_use_sequence}"
        self.append_log(name, {
            "type": "assistant",
            "message": {"content": [{
                "type": "tool_use",
                "name": "Bash",
                "id": tool_id,
                "input": {"command": command},
            }]},
        })
        self.append_log(name, {
            "type": "user",
            "message": {"content": [{
                "type": "tool_result",
                "tool_use_id": tool_id,
                "content": "ok" if ok else "Exit code 1\nfailed",
                "is_error": not ok,
            }]},
        })

    def record_commit(self, name, sha, gate=None, gate_ok=True, after_gate=None):
        if gate:
            self.append_bash(name, gate, ok=gate_ok)
        if after_gate:
            self.append_bash(name, after_gate, ok=True)
        self.append_bash(name, "git commit -m 'fixture commit'", ok=True)
        self.append_text(name, f"[chainsaw {sha[:10]}]")

    def prepare_committed_task(self, *, gate=None, gate_ok=True, after_gate=None,
                               trailer=False):
        task_id = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task_id))
        sha = self.commit_with_trailer() if trailer else self.commit_file()
        self.record_commit("worker", sha, gate=gate, gate_ok=gate_ok,
                           after_gate=after_gate)
        return task_id, sha

    def start_daemon(self, lead="lead"):
        command = [*self.supervisor_command, "--run-dir", str(self.run_dir),
                   "daemon", "--lead", lead]
        process = subprocess.Popen(
            command,
            text=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            env=self.env,
        )

        def cleanup():
            if process.poll() is None:
                self.cli("stop")
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.terminate()
                    process.wait(timeout=5)

        self.addCleanup(cleanup)
        return process

    def wait_for_state(self, needle, timeout=12):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            last = self.assert_success(self.cli("state"))
            if needle in last.stdout:
                return last
            time.sleep(0.1)
        self.fail(f"state never contained {needle!r}:\n{last and last.stdout}")
