"""Fixtures for exercising the supervisor only through its process boundary."""

import fcntl
import json
import os
import shlex
import shutil
import sqlite3
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[1]
MANIFEST = PROJECT_ROOT / "Cargo.toml"
BINARY = PROJECT_ROOT / "target" / "debug" / "chainsaw"
FAKE_HERDR = PROJECT_ROOT / "tests" / "fake_herdr.py"

_configured_command = os.environ.get("CHAINSAW_SUPERVISOR_COMMAND")
if _configured_command:
    SUPERVISOR_COMMAND = shlex.split(_configured_command)
else:
    subprocess.run(
        ["cargo", "build", "--manifest-path", str(MANIFEST)],
        cwd=PROJECT_ROOT,
        check=True,
    )
    SUPERVISOR_COMMAND = [str(BINARY)]
class SupervisorContractCase(unittest.TestCase):
    """An isolated installation, Git repository, and Herdr per test.

    The Herdr is `tests/fake_herdr.py`, put on PATH as `herdr`; the supervisor
    drives it exactly as it drives the real one.
    """

    maxDiff = None

    #: Basename of the run directory, so a case can exercise an awkward one.
    run_dir_name = "run"

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="chainsaw-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.sandbox = Path(self.temporary.name)
        self.run_dir = self.sandbox / self.run_dir_name
        self.home = self.sandbox / "home"
        self.runtime_state_path = self.sandbox / "herdr-state.json"
        self.run_dir.mkdir()
        self.home.mkdir()
        bin_dir = self.sandbox / "bin"
        bin_dir.mkdir()
        (bin_dir / "herdr").symlink_to(FAKE_HERDR)

        self.env = os.environ.copy()
        # The developer's own global settings must not leak into a run.
        self.env.pop("XDG_CONFIG_HOME", None)
        self.env.pop("CHAINSAW_CONFIG", None)
        self.env.update({
            "HOME": str(self.home),
            "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
            "HERDR_WORKSPACE_ID": "workspace-1",
            "HERDR_TAB_ID": "tab-0",
            "FAKE_HERDR_STATE": str(self.runtime_state_path),
            "GIT_AUTHOR_NAME": "Chainsaw Tests",
            "GIT_AUTHOR_EMAIL": "chainsaw-tests@example.invalid",
            "GIT_COMMITTER_NAME": "Chainsaw Tests",
            "GIT_COMMITTER_EMAIL": "chainsaw-tests@example.invalid",
        })
        self.supervisor_command = self._private_supervisor_command()
        self._daemons = []

        self.git("init", "-q", "-b", "master")
        (self.run_dir / "seed.txt").write_text("initial\n")
        self.git("add", "seed.txt")
        self.git("commit", "-q", "-m", "chore: initial fixture")
        self._tool_use_sequence = 0
        self._transcripts_dirs = {}

    def _private_supervisor_command(self):
        """Run a private copy of the binary, so a rebuild in target/ during the
        run cannot replace it under a live daemon."""
        if SUPERVISOR_COMMAND != [str(BINARY)]:
            return SUPERVISOR_COMMAND
        private = self.sandbox / BINARY.name
        shutil.copy2(BINARY, private)
        return [str(private)]

    @property
    def transcripts_dir(self):
        return self.transcripts_dir_for(self.run_dir)

    def transcripts_dir_for(self, run_dir):
        """Ask the supervisor where it keeps transcripts; never reimplement its rule."""
        if run_dir not in self._transcripts_dirs:
            result = subprocess.run(
                [*self.supervisor_command, "--run-dir", str(run_dir), "transcripts-dir"],
                text=True, capture_output=True, env=self.env, timeout=30,
            )
            self.assert_success(result)
            self._transcripts_dirs[run_dir] = Path(result.stdout.strip())
        return self._transcripts_dirs[run_dir]

    def write_supervisor_db(self, sql, *params):
        """Move a durable fact the CLI cannot, such as a timestamp into the past."""
        with sqlite3.connect(self.transcripts_dir / "chainsaw-supervisor.db") as database:
            database.execute(sql, params)

    def write_lead_transcript(self, context, session_id="session-lead"):
        """Give the lead a transcript whose last turn carried this much context."""
        log = self.transcripts_dir / f"{session_id}.jsonl"
        log.parent.mkdir(parents=True, exist_ok=True)
        log.write_text(json.dumps({
            "type": "assistant",
            "message": {"usage": {"input_tokens": context}},
        }) + "\n")

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

    def write_settings(self, text):
        """Put a chainsaw.toml in the run directory; the next process reads it."""
        (self.run_dir / "chainsaw.toml").write_text(text)

    def write_local_settings(self, text):
        """Put a chainsaw.local.toml in the run directory, over chainsaw.toml."""
        (self.run_dir / "chainsaw.local.toml").write_text(text)

    def write_global_settings(self, text, config_home=None):
        """Put the global chainsaw.toml under `config_home`, the sandbox home's
        .config by default; every run directory reads it first."""
        directory = Path(config_home or self.home / ".config") / "chainsaw"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "chainsaw.toml").write_text(text)

    def session_agent(self, name):
        """The agent the live session row named `name` was launched with."""
        with sqlite3.connect(self.transcripts_dir / "chainsaw-supervisor.db") as database:
            (agent,) = database.execute(
                "select agent from sessions where name=? and stopped_at is null", (name,)
            ).fetchone()
        return agent

    def launch_args(self, name):
        """The agent flags the runtime was handed when it last started `name`."""
        return next(
            operation["args"] for operation in reversed(self.runtime_operations())
            if operation["operation"] == "start" and operation["session_id"] == name
        )

    def start_commentator(self):
        self.assert_success(self.cli(
            "start-commentator", "--role-prompt", str(self.run_dir / "commentator.md"),
        ))
        return next(
            name for name in self.herdr_state()["agents"]
            if name.startswith("commentator-")
        )

    def dispatch(self, task_id, name="worker"):
        return self.cli("dispatch", str(task_id), "--to", name)

    def herdr_state(self):
        return json.loads(self.runtime_state_path.read_text())

    def update_herdr_state(self, **updates):
        state = self.herdr_state() if self.runtime_state_path.exists() else {
            "agents": {}, "panes": {}, "sequence": 0, "drop_prompts": 0,
            "operations": [],
        }
        state.update(updates)
        self.runtime_state_path.write_text(json.dumps(state, sort_keys=True))

    def runtime_operations(self):
        return self.herdr_state()["operations"]

    def prompts_to(self, name):
        return [
            operation["text"] for operation in self.runtime_operations()
            if operation["operation"] == "prompt"
            and operation["session_id"] == name
        ]

    def set_agent_status(self, name, status):
        """Mark a session busy or idle; a busy one queues prompts instead of answering.

        The supervisor polls this file once a second, so take its lock and land the
        new contents atomically rather than racing its read-modify-write.
        """
        lock_path = self.runtime_state_path.with_suffix(".lock")
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        with lock_path.open("a+") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            state = self.herdr_state()
            state["agents"][name]["status"] = status
            temporary = self.runtime_state_path.with_suffix(".tmp")
            temporary.write_text(json.dumps(state, sort_keys=True))
            temporary.replace(self.runtime_state_path)

    def session_transcript(self, name):
        """Where the session's agent writes; the fake Herdr settled it at launch."""
        return Path(self.herdr_state()["agents"][name]["transcript"])

    def append_entry(self, name, entry):
        path = self.session_transcript(name)
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a") as stream:
            stream.write(json.dumps(entry, separators=(",", ":")) + "\n")

    def append_text(self, name, text):
        self.append_entry(name, {
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": text}]},
        })

    def append_usage(self, name, input_tokens=0, cache_read=0, cache_creation=0,
                     sidechain=False):
        self.append_entry(name, {
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

    def append_codex_usage(self, name, input_tokens, cached_input_tokens=0,
                           thread_input_tokens=None):
        """What Codex records after a response handed `input_tokens` of context,
        `cached_input_tokens` of which came from cache; the thread total is the
        running sum that is not the context."""
        thread_input_tokens = input_tokens if thread_input_tokens is None else thread_input_tokens
        self.append_entry(name, {
            "type": "token_usage_record",
            "payload": {
                "usage": {
                    "input_tokens": input_tokens,
                    "cached_input_tokens": cached_input_tokens,
                    "output_tokens": 60,
                },
                "thread_token_usage": {
                    "input_tokens": thread_input_tokens,
                    "cached_input_tokens": cached_input_tokens,
                },
            },
        })

    def append_bash(self, name, command, ok=True):
        self._tool_use_sequence += 1
        tool_id = f"tool-{self._tool_use_sequence}"
        self.append_entry(name, {
            "type": "assistant",
            "message": {"content": [{
                "type": "tool_use",
                "name": "Bash",
                "id": tool_id,
                "input": {"command": command},
            }]},
        })
        self.append_entry(name, {
            "type": "user",
            "message": {"content": [{
                "type": "tool_result",
                "tool_use_id": tool_id,
                "content": "ok" if ok else "Exit code 1\nfailed",
                "is_error": not ok,
            }]},
        })

    def record_commit(self, name, sha):
        self.append_bash(name, "git commit -m 'fixture commit'", ok=True)
        self.append_text(name, f"[chainsaw {sha[:10]}]")

    def prepare_committed_task(self, *, trailer=False):
        task_id = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task_id))
        self.observe_in_flight(task_id)
        sha = self.commit_with_trailer() if trailer else self.commit_file()
        self.record_commit("worker", sha)
        return task_id, sha

    def observe_in_flight(self, task_id, name="worker"):
        """Grow the implementer log and let the daemon record the flight baseline."""
        self.append_text(name, "fixture work started")
        daemon = self.start_daemon()
        state = self.wait_for_state(f"{task_id} in_flight")
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)
        return state

    def start_daemon(self, lead="lead", session_id=None, expected_exit=0):
        """Start a daemon that must have exited with `expected_exit` by teardown."""
        session_id = session_id or f"session-{lead}"
        command = [*self.supervisor_command, "--run-dir", str(self.run_dir),
                   "daemon", "--lead", lead, "--session-id", session_id,
                   "--poll-interval-ms", "10"]
        stderr_path = self.sandbox / f"daemon-{len(self._daemons)}.stderr"
        process = subprocess.Popen(
            command,
            text=True,
            stdout=subprocess.DEVNULL,
            stderr=stderr_path.open("w"),
            env=self.env,
        )
        process.stderr_path = stderr_path
        self._daemons.append(process)

        def cleanup():
            if process.poll() is None:
                self.cli("stop")
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.terminate()
                    process.wait(timeout=5)
            if process.returncode != expected_exit:
                self.fail(
                    f"daemon did not exit with {expected_exit}:{self.daemon_report()}"
                )

        self.addCleanup(cleanup)
        return process

    def wait_for_state(self, text, timeout=12):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            last = self.assert_success(self.cli("state"))
            if text in last.stdout:
                return last
            time.sleep(0.1)
        self.fail(
            f"state never contained {text!r}:\n{last and last.stdout}"
            f"{self.daemon_report()}"
        )

    def daemon_report(self):
        """What every daemon of this test has said and whether it is still up."""
        lines = []
        for process in self._daemons:
            status = ("running" if process.poll() is None
                      else f"exited with {process.returncode}")
            stderr = process.stderr_path.read_text().strip()
            lines.append(f"daemon pid {process.pid} {status}; stderr:\n{stderr or '(empty)'}")
        return "\n" + "\n".join(lines) if lines else ""
