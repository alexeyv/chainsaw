"""What a real coding agent does behind a fake runtime, shared by `fake_herdr.py`
and `fake_orca.py`: where each CLI writes its transcript, what a prompt and a
reply look like in that CLI's format, and the locked state file the fakes keep
for the tests to read.

An agent is a dict in the state's `agents`: `session_id`, `status`, `run_dir`,
`agent` (its kind), `transcript`, `queued`, and `busy_until`, the moment its
current turn ends. A test marks an agent busy through `status`; a fake marks it
busy for a moment after each prompt, as a real agent is. While the state's
`hold_transcript` is set, an agent works without writing, as Cursor does until
its first reply; what it would have written waits in `held`.
"""

import fcntl
import json
import os
import re
import time
from datetime import datetime, timezone
from pathlib import Path

#: How long an agent is mid-turn after a prompt lands. Long enough for the
#: supervisor's first probe to find it busy, as it would find a real one.
TURN_SECONDS = 0.75

#: The executable each kind of agent is, as a terminal runs it.
PROGRAMS = {"claude": "claude", "codex": "codex", "cursor": "cursor-agent"}


def kind_running(program):
    """The kind of agent behind an executable."""
    for kind, name in PROGRAMS.items():
        if name == program:
            return kind
    raise SystemExit(f"fake runtime cannot stand in for {program}")


def new_agent(kind, run_dir, session_id):
    return {
        "session_id": session_id,
        "status": "idle",
        "run_dir": run_dir,
        "agent": kind,
        "transcript": str(transcript_for(kind, run_dir, session_id)),
        "queued": [],
        "busy_until": 0,
    }


def is_busy(agent):
    return agent["status"] == "busy" or time.time() < agent.get("busy_until", 0)


def begin_turn(agent):
    agent["busy_until"] = time.time() + TURN_SECONDS


def end_turn(agent):
    agent["status"] = "idle"
    agent["busy_until"] = 0
    agent["queued"] = []


# --- what a real agent does


def open_transcript(current, agent, args):
    """What an agent does the moment it starts, given `args`: Codex names the
    session and its working directory, and every agent takes the prompt after
    `--` on its command line as a real one would."""
    if agent["agent"] == "codex":
        append_entry(agent, {
            "type": "session_meta",
            "payload": {"id": agent["session_id"], "cwd": agent["run_dir"]},
        })
    if "--" in args and args.index("--") + 1 < len(args):
        deliver(current, agent, args[args.index("--") + 1])


def deliver(current, agent, text):
    """A real agent picks up a prompt and, when the test says so, answers it.
    While the test holds the transcript, it works without writing, as Cursor
    does until its first reply."""
    entries = [prompt_entry(agent["agent"], text)]
    reply_text = current.get("reply_on_prompt")
    if reply_text is not None:
        entries.append(reply_entry(agent["agent"], reply_text))
    if agent["agent"] == "cursor":
        drop_turn_ended(agent)
    if current.get("hold_transcript"):
        agent.setdefault("held", []).extend(entries)
    else:
        append_entry(agent, entries)


def drop_turn_ended(agent):
    """Cursor ends its transcript with a `turn_ended` line that the next turn
    removes before writing, so the file shrinks by that line as the turn begins."""
    path = Path(agent["transcript"])
    if not path.exists():
        return
    lines = path.read_text().splitlines(keepends=True)
    if lines and json.loads(lines[-1]).get("type") == "turn_ended":
        path.write_text("".join(lines[:-1]))


def flush_held(current, agent):
    """A late-writing agent's transcript catches up once the test releases it."""
    if current.get("hold_transcript") or not agent.get("held"):
        return
    append_entry(agent, agent.pop("held"))


def release_transcripts(current):
    """The reply comes: every agent writes what it was holding back, whether or
    not anyone asks the runtime about it meanwhile."""
    current["hold_transcript"] = False
    for agent in current["agents"].values():
        flush_held(current, agent)


def enqueue(agent, text):
    """A busy agent takes the prompt for later."""
    agent.setdefault("queued", []).append(text)
    queued = queued_prompt_entry(agent["agent"], text)
    if queued is not None:
        append_entry(agent, queued)


def drain_queue(current, agent):
    """A real agent writes what it held back and, once idle, works through what
    it queued while busy."""
    if agent is None:
        return
    flush_held(current, agent)
    if is_busy(agent):
        return
    for text in agent.get("queued", []):
        deliver(current, agent, text)
    agent["queued"] = []


# --- where and how each agent writes


def transcript_for(kind, run_dir, session_id, home=None):
    """Where the agent of `kind` writes the transcript of `session_id` started in
    `run_dir`, under `home` (the fake's own when unnamed)."""
    home = Path(home or os.environ["HOME"])
    if kind == "claude":
        project = os.path.realpath(run_dir).replace("/", "-").replace(".", "-")
        return home / ".claude" / "projects" / project / f"{session_id}.jsonl"
    if kind == "codex":
        now = datetime.now(timezone.utc)
        codex_home = Path(os.environ.get("CODEX_HOME") or home / ".codex")
        return (codex_home / "sessions" / now.strftime("%Y/%m/%d")
                / f"rollout-{now.strftime('%Y-%m-%dT%H-%M-%S')}-{session_id}.jsonl")
    if kind == "cursor":
        project = re.sub(r"[^A-Za-z0-9]+", "-", os.path.realpath(run_dir)).strip("-")
        return (home / ".cursor" / "projects" / project / "agent-transcripts" / session_id
                / f"{session_id}.jsonl")
    raise SystemExit(f"fake runtime cannot stand in for {kind}")


def prompt_entry(kind, text):
    if kind == "codex":
        return codex_message("user", "input_text", text)
    if kind == "cursor":
        stamp = datetime.now(timezone.utc).strftime("%A, %B %d, %Y %I:%M %p")
        return cursor_message(
            "user", f"<timestamp>{stamp}</timestamp>\n<user_query>\n{text}\n</user_query>"
        )
    return {"type": "user", "message": {"content": text}}


def queued_prompt_entry(kind, text):
    """Codex and Cursor write nothing for a prompt waiting behind the current turn."""
    if kind in ("codex", "cursor"):
        return None
    return {
        "type": "queue-operation",
        "operation": "enqueue",
        "content": text,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }


def reply_entry(kind, text):
    if kind == "codex":
        return codex_message("assistant", "output_text", text)
    if kind == "cursor":
        return [cursor_message("assistant", text), {"type": "turn_ended", "status": "success"}]
    return {"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}}


def codex_message(role, block_type, text):
    return {
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": role,
            "content": [{"type": block_type, "text": text}],
        },
    }


def cursor_message(role, text):
    return {
        "role": role,
        "message": {"content": [{"type": "text", "text": text}]},
    }


def append_entry(agent, entry):
    """Write one entry, or each of a list of them (however nested), to the agent's
    transcript."""
    path = Path(agent["transcript"])
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as transcript:
        for item in flattened(entry):
            transcript.write(json.dumps(item, separators=(",", ":")) + "\n")


def flattened(entry):
    if not isinstance(entry, list):
        return [entry]
    return [item for element in entry for item in flattened(element)]


# --- plumbing


def record(current, operation, session_id, **details):
    """What the supervisor asked, under the id the runtime knows the session by."""
    current["operations"].append({"operation": operation, "session_id": session_id, **details})


def parse_flags(arguments):
    """`--flag value` pairs, bare `--flag`s as True, and everything after `--`."""
    flags = {}
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == "--":
            flags["--"] = arguments[index + 1:]
            break
        if index + 1 < len(arguments) and not arguments[index + 1].startswith("--"):
            flags[argument] = arguments[index + 1]
            index += 2
        else:
            flags[argument] = True
            index += 1
    return flags


class state:
    """The state file named by the environment variable `variable`, held under
    an exclusive lock and written back atomically. The tests take the same lock
    when they edit it."""

    def __init__(self, variable, empty):
        self.path = Path(os.environ[variable])
        self.empty = empty

    def __enter__(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.lock = self.path.with_suffix(".lock").open("a+")
        fcntl.flock(self.lock, fcntl.LOCK_EX)
        self.current = (json.loads(self.path.read_text()) if self.path.exists()
                        else json.loads(json.dumps(self.empty)))
        return self.current

    def __exit__(self, exception_type, exception, traceback):
        if exception is None:
            temporary = self.path.with_suffix(".tmp")
            temporary.write_text(json.dumps(self.current, sort_keys=True))
            temporary.replace(self.path)
        fcntl.flock(self.lock, fcntl.LOCK_UN)
        self.lock.close()
        return False
