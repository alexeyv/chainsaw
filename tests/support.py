"""Fixtures for exercising the supervisor only through its process boundary."""

import io
import json
import os
import shlex
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import zipfile
from contextlib import closing, contextmanager, suppress
from functools import cache
from pathlib import Path

from tests import fake_agent


PROJECT_ROOT = Path(__file__).resolve().parents[1]
#: The skill the repository ships, and the supervisor crate inside it.
SKILL = PROJECT_ROOT / "skills" / "chainsaw"
CRATE = SKILL / "coordinator"
MANIFEST = CRATE / "Cargo.toml"
BINARY = CRATE / "target" / "debug" / ("chainsaw.exe" if os.name == "nt" else "chainsaw")
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


def put_on_path(bin_dir, name, script):
    """Make `script` runnable as the program `name` from `bin_dir`. POSIX runs it
    through its shebang. Windows finds only `.exe` files on a bare name, so there
    it becomes one."""
    if os.name != "nt":
        (bin_dir / name).symlink_to(script)
        return
    (bin_dir / f"{name}.exe").write_bytes(windows_launcher(script))


@cache
def windows_launcher(script):
    """A `.exe` running `script` under this interpreter: distlib's console
    launcher (vendored by pip in the base installation, so a venv without pip
    has it too), then the interpreter as its shebang, then a zip whose
    `__main__.py` runs the script."""
    launcher = (Path(sys.base_prefix) / "Lib" / "site-packages" / "pip" / "_vendor"
                / "distlib" / "t64.exe").read_bytes()
    archive = io.BytesIO()
    with zipfile.ZipFile(archive, "w") as bundle:
        bundle.writestr(
            "__main__.py",
            f"import runpy\nrunpy.run_path({str(script)!r}, run_name='__main__')\n",
        )
    shebang = f'#!"{sys.executable}"\r\n'.encode()
    return launcher + shebang + archive.getvalue()


if os.name == "nt":
    import ctypes
    from ctypes import wintypes

    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _kernel32.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
    _kernel32.OpenProcess.restype = ctypes.c_void_p
    _kernel32.WaitForSingleObject.argtypes = (ctypes.c_void_p, wintypes.DWORD)
    _kernel32.WaitForSingleObject.restype = wintypes.DWORD
    _kernel32.TerminateProcess.argtypes = (ctypes.c_void_p, wintypes.UINT)
    _kernel32.TerminateProcess.restype = wintypes.BOOL
    _kernel32.CloseHandle.argtypes = (ctypes.c_void_p,)
    _kernel32.CloseHandle.restype = wintypes.BOOL
    _SYNCHRONIZE = 0x0010_0000
    _PROCESS_TERMINATE = 0x0001
    _WAIT_OBJECT_0 = 0


def process_exits(pid, timeout_ms=10_000):
    """Whether a process that said it is exiting is gone within the timeout.
    Windows will not remove the sandbox while the process still holds its
    files; elsewhere its last word is enough."""
    if os.name != "nt":
        return True
    handle = _kernel32.OpenProcess(_SYNCHRONIZE, False, pid)
    if not handle:
        return True
    try:
        return _kernel32.WaitForSingleObject(handle, timeout_ms) == _WAIT_OBJECT_0
    finally:
        _kernel32.CloseHandle(handle)


def kill_process(pid):
    """Kill a process that is not this one's child, if it is still there."""
    if os.name != "nt":
        with suppress(ProcessLookupError):
            os.kill(pid, signal.SIGKILL)
        return
    handle = _kernel32.OpenProcess(_PROCESS_TERMINATE, False, pid)
    if handle:
        try:
            _kernel32.TerminateProcess(handle, 1)
        finally:
            _kernel32.CloseHandle(handle)


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
        put_on_path(bin_dir, self.runtime, FAKE_RUNTIMES[self.runtime])

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
            "USERPROFILE": str(self.home),
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
        self._state_dirs = {}

    def _runtime_environment(self):
        """What the lead's terminal exports, and where its fake keeps state."""
        if self.runtime == "herdr":
            return {
                "HERDR_WORKSPACE_ID": "workspace-1",
                "HERDR_TAB_ID": "tab-0",
                "HERDR_PANE_ID": "pane-lead",
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
    def state_dir(self):
        return self.state_dir_for(self.run_dir)

    def state_dir_for(self, run_dir):
        """Ask the supervisor where it keeps a run's state; never reimplement its rule."""
        if run_dir not in self._state_dirs:
            result = subprocess.run(
                [*self.supervisor_command, "--run-dir", str(run_dir), "state-dir"],
                text=True, encoding="utf-8", capture_output=True, env=self.env, timeout=30,
            )
            self.assert_success(result)
            self._state_dirs[run_dir] = Path(result.stdout.strip())
        return self._state_dirs[run_dir]

    @contextmanager
    def supervisor_db(self):
        """The supervisor's database, committed and then closed: Windows will not
        remove the sandbox while a connection is left open."""
        with closing(sqlite3.connect(self.state_dir / "chainsaw-supervisor.db")) as database:
            with database:
                yield database

    def recorded_commit(self, task_id):
        """The commit sha the supervisor has on record for `task_id`."""
        with self.supervisor_db() as database:
            (sha,) = database.execute(
                "select commit_sha from tasks where id=?", (task_id,)
            ).fetchone()
        return sha

    def write_supervisor_db(self, sql, *params):
        """Move a durable fact the CLI cannot, such as a timestamp into the past."""
        with self.supervisor_db() as database:
            database.execute(sql, params)

    def claude_transcript(self, session_id, run_dir=None):
        """Where Claude Code writes the transcript of `session_id` started in `run_dir`,
        which is where the supervisor reads the transcript of a session on Claude."""
        return fake_agent.transcript_for(
            "claude", run_dir or self.run_dir, session_id, home=self.home,
        )

    def write_lead_transcript(self, context, session_id="session-lead"):
        """Give the lead a transcript whose last turn carried this much context."""
        log = self.claude_transcript(session_id)
        log.parent.mkdir(parents=True, exist_ok=True)
        log.write_text(newline="", data=json.dumps({
            "type": "assistant",
            "message": {"usage": {"input_tokens": context}},
        }) + "\n")

    def cli(self, *args, input_text=None, timeout=30):
        command = [*self.supervisor_command, "--run-dir", str(self.run_dir), *map(str, args)]
        return subprocess.run(
            command,
            input=input_text,
            text=True, encoding="utf-8",
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
            text=True, encoding="utf-8",
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
        with self.supervisor_db() as database:
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
        with self.supervisor_db() as database:
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
        registry = json.loads((self.state_dir / ORCA_REGISTRY_FILE_NAME).read_text())
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
            fake_agent.lock(lock)
            try:
                state = self.runtime_state()
                mutate(state)
                fake_agent.write_atomically(
                    self.runtime_state_path, json.dumps(state, sort_keys=True)
                )
            finally:
                fake_agent.unlock(lock)

    def session_transcript(self, name):
        """Where the session's agent writes; the fake runtime settled it at launch."""
        return Path(self.session_state(name)["transcript"])

    def stop_session(self, name):
        """Mark the session stopped, as relaunching its name or `stop` does."""
        self.write_supervisor_db(
            "update sessions set stopped_at=started_at where name=? and stopped_at is null", name
        )

    def stop_session_and_remove_transcript(self, name):
        """Stop the session, then delete its transcript, as Claude Code's
        cleanup of old transcripts does long after."""
        transcript = self.session_transcript(name)
        self.stop_session(name)
        transcript.unlink()

    def append_entry(self, name, entry):
        path = self.session_transcript(name)
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", newline="") as stream:
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

    def prepare_committed_task(self):
        task_id = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task_id))
        self.observe_in_flight(task_id)
        sha = self.commit_file()
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

    def start_daemon(self, lead="lead", session_id=None, agent=None, expected_exit=0,
                     lead_transcript=True):
        """Start a daemon that must have exited with `expected_exit` by teardown.
        The lead runs on `agent` when named, otherwise on the daemon's default. It
        registers with the transcript its agent is already writing, so one is begun
        for it unless the case says otherwise or has written its own."""
        session_id = session_id or f"session-{lead}"
        if lead_transcript:
            self.begin_lead_transcript(session_id, agent or "claude")
        command = [*self.supervisor_command, "--run-dir", str(self.run_dir),
                   "daemon", "--lead", lead, "--session-id", session_id,
                   *(["--agent", agent] if agent else []),
                   "--poll-interval-ms", "10"]
        stderr_path = self.sandbox / f"daemon-{len(self._daemons)}.stderr"
        # The daemon keeps its own handle; ours closes at once, so nothing of the
        # test's holds the sandbox open when it is removed.
        with stderr_path.open("w") as stderr:
            process = subprocess.Popen(
                command,
                text=True, encoding="utf-8",
                stdout=subprocess.DEVNULL,
                stderr=stderr,
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

    def begin_lead_transcript(self, session_id, kind="claude"):
        """The transcript the lead's agent is writing, unless the case wrote its own."""
        if not list(fake_agent.transcripts_root(kind, self.home).rglob(f"*{session_id}.jsonl")):
            log = fake_agent.transcript_for(kind, self.run_dir, session_id, home=self.home)
            log.parent.mkdir(parents=True, exist_ok=True)
            log.touch()

    def next_daemon_seen_at(self, previous, timeout=10):
        """The daemon's next poll stamp after `previous` (None: its first)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self.supervisor_db() as database:
                (seen,) = database.execute(
                    "select daemon_seen_at from run where id=1"
                ).fetchone()
            if seen is not None and seen != previous:
                return seen
            time.sleep(0.05)
        self.fail(f"the daemon has not polled since {previous}")

    @contextmanager
    def prompt_lock_held(self):
        """Hold the lock every prompt is sent under, as a prompt in flight does."""
        with (self.state_dir / "chainsaw-supervisor.db.prompt-lock").open("a+") as lock:
            fake_agent.lock(lock)
            try:
                yield
            finally:
                fake_agent.unlock(lock)

    def stop_detached_daemons_later(self):
        """Stop, at cleanup, every daemon whose start is recorded from now on.
        A daemon `daemon --detach` starts is not this test's child, so it is
        found by its start event, whether the command returned, failed or timed
        out, and must have exited before the sandbox is removed."""
        # ponytail: a daemon that never recorded its start is not found.
        database = self.state_dir / "chainsaw-supervisor.db"
        boundary = 0
        if database.exists():
            with self.supervisor_db() as connection:
                (boundary,) = connection.execute(
                    "select coalesce(max(id), 0) from run_events"
                ).fetchone()
        self.addCleanup(self._stop_daemons_started_after, boundary)

    def _stop_daemons_started_after(self, boundary):
        if not (self.state_dir / "chainsaw-supervisor.db").exists():
            return
        # `pid 0` is a case's stand-in for another daemon.
        started = self._daemon_pids("daemon-start", boundary) - {0}
        if not started:
            return
        try:
            stop = self.cli("stop")
            said = f"exit {stop.returncode}:\n{stop.stdout}{stop.stderr}"
        except subprocess.TimeoutExpired:
            said = "it timed out"
        deadline = time.monotonic() + 10
        while not started <= self._daemon_pids("daemon-exit", boundary):
            if time.monotonic() > deadline:
                break
            time.sleep(0.05)
        exited = self._daemon_pids("daemon-exit", boundary)
        stuck = sorted(pid for pid in started if pid not in exited or not process_exits(pid))
        for pid in stuck:
            kill_process(pid)
        if stuck:
            self.fail(
                f"detached daemons {stuck} did not exit after stop; killed if still running;"
                f" stop gave {said}"
            )

    def _daemon_pids(self, kind, boundary):
        with self.supervisor_db() as database:
            return {
                int(detail.removeprefix("pid ")) for (detail,) in database.execute(
                    "select detail from run_events where kind=? and id>?", (kind, boundary)
                )
            }

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
