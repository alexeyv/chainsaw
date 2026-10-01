"""Fixtures for exercising the supervisor only through its process boundary."""

import fcntl
import json
import os
import shlex
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path

from tests import fake_agent


PROJECT_ROOT = Path(__file__).resolve().parents[1]
MANIFEST = PROJECT_ROOT / "Cargo.toml"
BINARY = PROJECT_ROOT / "target" / "debug" / "chainsaw"
#: The fake standing in for each terminal runtime, put on PATH under its name.
FAKE_RUNTIMES = {
    "herdr": PROJECT_ROOT / "tests" / "fake_herdr.py",
    "orca": PROJECT_ROOT / "tests" / "fake_orca.py",
}
#: The first prompt a session is launched on unless a case names another.
READING_TURN = "Read the fixture files, then stop and wait for the task."
#: Where the supervisor maps session names to the Orca terminals it opened.
ORCA_REGISTRY_FILE_NAME = "chainsaw-orca-terminals.json"

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
    """An isolated installation, Git repository, and terminal runtime per test.

    The runtime is the fake `runtime` names, put on PATH as `herdr` or `orca`
    with the environment that terminal would give the lead; the supervisor
    drives it exactly as it drives the real one.
    """

    maxDiff = None

    #: Basename of the run directory, so a case can exercise an awkward one.
    run_dir_name = "run"

    #: The terminal the run lives in: "herdr" or "orca".
    runtime = "herdr"

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="chainsaw-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.sandbox = Path(self.temporary.name)
        self.run_dir = self.sandbox / self.run_dir_name
        self.home = self.sandbox / "home"
        self.runtime_state_path = self.sandbox / f"{self.runtime}-state.json"
        self.run_dir.mkdir()
        self.home.mkdir()
        bin_dir = self.sandbox / "bin"
        bin_dir.mkdir()
        (bin_dir / self.runtime).symlink_to(FAKE_RUNTIMES[self.runtime])

        self.env = os.environ.copy()
        # The developer's own global settings must not leak into a run.
        self.env.pop("XDG_CONFIG_HOME", None)
        self.env.pop("CHAINSAW_CONFIG", None)
        # Only the runtime under test may be visible, whichever terminal the
        # suite itself runs from.
        for variable in ("HERDR_ENV", "HERDR_WORKSPACE_ID", "HERDR_TAB_ID", "HERDR_PANE_ID",
                         "ORCA_TERMINAL_HANDLE", "ORCA_TAB_ID", "ORCA_WORKTREE_ID"):
            self.env.pop(variable, None)
        self.env.update({
            "HOME": str(self.home),
            "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
            "GIT_AUTHOR_NAME": "Chainsaw Tests",
            "GIT_AUTHOR_EMAIL": "chainsaw-tests@example.invalid",
            "GIT_COMMITTER_NAME": "Chainsaw Tests",
            "GIT_COMMITTER_EMAIL": "chainsaw-tests@example.invalid",
            **self._runtime_environment(),
        })
        self.supervisor_command = self._private_supervisor_command()
        self._daemons = []

        self.git("init", "-q", "-b", "master")
        (self.run_dir / "seed.txt").write_text("initial\n")
        self.git("add", "seed.txt")
        self.git("commit", "-q", "-m", "chore: initial fixture")
        self._tool_use_sequence = 0
        self._transcripts_dirs = {}

    def _runtime_environment(self):
        """What the lead's terminal exports, and where its fake keeps state."""
        if self.runtime == "herdr":
            return {
                "HERDR_WORKSPACE_ID": "workspace-1",
                "HERDR_TAB_ID": "tab-0",
                "FAKE_HERDR_STATE": str(self.runtime_state_path),
            }
        return {
            "ORCA_TERMINAL_HANDLE": "term-lead",
            "ORCA_TAB_ID": "tab-0",
            "FAKE_ORCA_STATE": str(self.runtime_state_path),
        }

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

    def launch(self, name="worker", prompt=None):
        """Launch `name` on its first prompt and let the turn that prompt starts
        end, as the reading turn a lead launches with has ended by the time it
        sends the task."""
        result = self.assert_success(self.cli("launch", name, prompt or READING_TURN))
        deadline = time.monotonic() + 5
        while fake_agent.is_busy(self.session_state(name)) and time.monotonic() < deadline:
            time.sleep(0.05)
        return result

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
        """The agent flags the runtime was handed when it last started `name`,
        without the prompt that follows them."""
        args = self._start_args(name)
        return args[:args.index("--")]

    def launch_prompt(self, name):
        """The prompt `name` was last started on, after `--` on its command line."""
        args = self._start_args(name)
        return args[args.index("--") + 1]

    def _start_args(self, name):
        return next(
            operation["args"] for operation in reversed(self.operations_on(name))
            if operation["operation"] == "start"
        )

    def start_commentator(self):
        """Start the commentator and return the name the supervisor gave it."""
        self.assert_success(self.cli(
            "start-commentator", "--role-prompt", str(self.run_dir / "commentator.md"),
        ))
        with sqlite3.connect(self.transcripts_dir / "chainsaw-supervisor.db") as database:
            (name,) = database.execute(
                "select name from sessions where role='commentator' and stopped_at is null"
                " order by id desc limit 1"
            ).fetchone()
        return name

    def dispatch(self, task_id, name="worker"):
        return self.cli("dispatch", str(task_id), "--to", name)

    def runtime_state(self):
        return json.loads(self.runtime_state_path.read_text())

    def update_runtime_state(self, **updates):
        state = self.runtime_state() if self.runtime_state_path.exists() else {
            "agents": {}, "panes": {}, "sequence": 0, "drop_prompts": 0,
            "operations": [],
        }
        state.update(updates)
        self.runtime_state_path.write_text(json.dumps(state, sort_keys=True))

    def session_handle(self, name):
        """What the runtime knows the session named `name` by. Herdr knows the
        name itself; Orca knows only the terminal, which the supervisor's own
        registry maps the name to."""
        if self.runtime == "herdr":
            return name
        registry = json.loads((self.transcripts_dir / ORCA_REGISTRY_FILE_NAME).read_text())
        return registry[name]["handle"]

    def session_state(self, name):
        """The fake runtime's record of the agent behind the session."""
        return self.runtime_state()["agents"][self.session_handle(name)]

    def forget_session(self, name):
        """Make the runtime lose the session, as when its pane is closed by hand."""
        state = self.runtime_state()
        del state["agents"][self.session_handle(name)]
        self.update_runtime_state(**state)

    def runtime_operations(self):
        return self.runtime_state()["operations"]

    def operations_on(self, name):
        """What the supervisor asked the runtime about the session named `name`."""
        handle = self.session_handle(name)
        return [
            operation for operation in self.runtime_operations()
            if operation["session_id"] == handle
        ]

    def prompts_to(self, name):
        return [
            operation["text"] for operation in self.operations_on(name)
            if operation["operation"] == "prompt"
        ]

    def set_agent_status(self, name, status):
        """Mark a session busy or idle; a busy one queues prompts instead of answering."""
        handle = self.session_handle(name)

        def mark(state):
            state["agents"][handle]["status"] = status
        self.edit_runtime_state_locked(mark)

    def hold_transcript(self, held):
        """Make every agent write its transcript late, as Cursor does: what a prompt
        would write is held back until the hold is released, when it lands at once."""
        def hold(state):
            if held:
                state["hold_transcript"] = True
            else:
                fake_agent.release_transcripts(state)
        self.edit_runtime_state_locked(hold)

    def release_transcript_after(self, seconds, first=None):
        """Release a held transcript from another thread while a CLI call waits on it,
        after doing `first` (what the agent did meanwhile, such as committing). The
        returned timer is joined by the caller once the call has returned."""
        def release():
            if first is not None:
                first()
            self.hold_transcript(False)
        timer = threading.Timer(seconds, release)
        timer.start()
        self.addCleanup(timer.cancel)
        return timer

    def edit_runtime_state_locked(self, mutate):
        """The supervisor polls this file once a second, so take its lock and land the
        new contents atomically rather than racing its read-modify-write."""
        lock_path = self.runtime_state_path.with_suffix(".lock")
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        with lock_path.open("a+") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            state = self.runtime_state()
            mutate(state)
            temporary = self.runtime_state_path.with_suffix(".tmp")
            temporary.write_text(json.dumps(state, sort_keys=True))
            temporary.replace(self.runtime_state_path)

    def session_transcript(self, name):
        """Where the session's agent writes; the fake runtime settled it at launch."""
        return Path(self.session_state(name)["transcript"])

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

    def start_daemon(self, lead="lead", session_id=None, expected_exit=0,
                     lead_transcript=True):
        """Start a daemon that must have exited with `expected_exit` by teardown.
        The lead registers with the transcript it is already writing, so one is
        begun for it unless the case says otherwise or has written its own."""
        session_id = session_id or f"session-{lead}"
        written = list((self.home / ".claude" / "projects").glob(f"*/{session_id}.jsonl"))
        if lead_transcript and not written:
            log = self.transcripts_dir / f"{session_id}.jsonl"
            log.parent.mkdir(parents=True, exist_ok=True)
            log.touch()
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
