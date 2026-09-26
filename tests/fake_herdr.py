#!/usr/bin/env python3
"""A `herdr` standing in at the process boundary: the supervisor runs it like the
real CLI, and it answers with the JSON shapes and exit codes the real one produces.

Its agents live in the JSON file `FAKE_HERDR_STATE` names, which the tests read
to see what the supervisor asked (`operations`) and edit to make an agent busy or
drop prompts. Each agent's transcript is written where that agent's real CLI
would write it, in that CLI's format, so the supervisor reads it back the same
way it reads a real session's.
"""

import fcntl
import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

EMPTY_STATE = {"agents": {}, "panes": {}, "sequence": 0, "drop_prompts": 0, "operations": []}


def main(argv):
    command, arguments = argv[:2], argv[2:]
    handlers = {
        ("tab", "create"): tab_create,
        ("pane", "split"): pane_split,
        ("agent", "start"): agent_start,
        ("agent", "get"): agent_get,
        ("agent", "prompt"): agent_prompt,
        ("agent", "send-keys"): agent_send_keys,
        ("agent", "wait"): agent_wait,
    }
    handler = handlers.get(tuple(command))
    if handler is None:
        print(f"unsupported: {' '.join(argv)}", file=sys.stderr)
        return 2
    return handler(arguments)


# --- the commands


def tab_create(arguments):
    flags = parse_flags(arguments)
    with state() as current:
        pane_id, tab_id = new_pane(current, flags["--cwd"])
    reply({"root_pane": {"pane_id": pane_id}, "tab": {"tab_id": tab_id}})
    return 0


def pane_split(arguments):
    flags = parse_flags(arguments)
    with state() as current:
        pane_id, _ = new_pane(current, flags["--cwd"])
    reply({"pane": {"pane_id": pane_id}})
    return 0


def agent_start(arguments):
    name, flags = arguments[0], parse_flags(arguments[1:])
    with state() as current:
        current["sequence"] += 1
        session_id = f"session-{name}-{current['sequence']}"
        run_dir = current["panes"][flags["--pane"]]["cwd"]
        kind = flags["--kind"]
        current["agents"][name] = {
            "session_id": session_id,
            "status": "idle",
            "run_dir": run_dir,
            "agent": kind,
            "transcript": str(transcript_for(kind, run_dir, session_id)),
            "queued": [],
        }
        record(current, "start", name, kind=kind, args=flags["--"])
    reply({"agent": {"agent_session": {"value": session_id}, "status": "idle"}})
    return 0


def agent_get(arguments):
    name = arguments[0]
    with state() as current:
        drain_queue(current, name)
        record(current, "query", name)
        agent = current["agents"].get(name)
    if agent is None:
        print("no such agent", file=sys.stderr)
        return 1
    reply({"agent": {"agent_session": {"value": agent["session_id"]}, "status": agent["status"]}})
    return 0


def agent_prompt(arguments):
    name, text = arguments[0], arguments[1]
    with state() as current:
        record(current, "prompt", name, text=text)
        if current["drop_prompts"] > 0:
            current["drop_prompts"] -= 1
            reply({"delivered": True})
            return 0
        agent = current["agents"].get(name)
        if agent is None:
            print("no such agent", file=sys.stderr)
            return 1
        if agent["status"] == "busy":
            agent.setdefault("queued", []).append(text)
            queued = queued_prompt_entry(agent["agent"], text)
            if queued is not None:
                append_entry(agent, queued)
        else:
            drain_queue(current, name)
            deliver(current, agent, text)
    reply({"delivered": True})
    return 0


def agent_send_keys(arguments):
    name = arguments[0]
    with state() as current:
        agent = current["agents"].get(name)
        if agent is None:
            print("no such agent", file=sys.stderr)
            return 1
        agent["status"] = "idle"
        agent["queued"] = []
        record(current, "interrupt", name)
    reply({"delivered": True})
    return 0


def agent_wait(arguments):
    name, flags = arguments[0], parse_flags(arguments[1:])
    timeout = int(flags["--timeout"]) / 1000
    with state() as current:
        record(current, "wait", name, timeout_ms=int(flags["--timeout"]))
    deadline = time.monotonic() + timeout
    while True:
        with state() as current:
            drain_queue(current, name)
            agent = current["agents"].get(name)
            idle = agent is None or agent["status"] != "busy"
        if idle or time.monotonic() >= deadline:
            break
        time.sleep(0.01)
    reply({"status": "idle" if idle else "busy"})
    return 0


# --- what a real agent does


def deliver(current, agent, text):
    """A real agent picks up a prompt and, when the test says so, answers it."""
    append_entry(agent, prompt_entry(agent["agent"], text))
    reply_text = current.get("reply_on_prompt")
    if reply_text is not None:
        append_entry(agent, reply_entry(agent["agent"], reply_text))


def drain_queue(current, name):
    """A real agent works through what it queued while busy once it is idle."""
    agent = current["agents"].get(name)
    if agent is None or agent["status"] == "busy":
        return
    for text in agent.get("queued", []):
        deliver(current, agent, text)
    agent["queued"] = []


# --- where and how each agent writes


def transcript_for(kind, run_dir, session_id):
    home = Path(os.environ["HOME"])
    if kind == "claude":
        project = os.path.realpath(run_dir).replace("/", "-").replace(".", "-")
        return home / ".claude" / "projects" / project / f"{session_id}.jsonl"
    if kind == "codex":
        now = datetime.now(timezone.utc)
        codex_home = Path(os.environ.get("CODEX_HOME") or home / ".codex")
        return (codex_home / "sessions" / now.strftime("%Y/%m/%d")
                / f"rollout-{now.strftime('%Y-%m-%dT%H-%M-%S')}-{session_id}.jsonl")
    raise SystemExit(f"fake herdr cannot stand in for {kind}")


def prompt_entry(kind, text):
    if kind == "codex":
        return codex_message("user", "input_text", text)
    return {"type": "user", "message": {"content": text}}


def queued_prompt_entry(kind, text):
    """Codex writes nothing for a prompt waiting behind the current turn."""
    if kind == "codex":
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


def append_entry(agent, entry):
    path = Path(agent["transcript"])
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as transcript:
        transcript.write(json.dumps(entry, separators=(",", ":")) + "\n")


# --- plumbing


def new_pane(current, cwd):
    number = len(current["panes"]) + 1
    pane_id, tab_id = f"pane-{number}", f"tab-{number}"
    current["panes"][pane_id] = {"cwd": cwd}
    return pane_id, tab_id


def record(current, operation, name, **details):
    current["operations"].append({"operation": operation, "session_id": name, **details})


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


def reply(result):
    print(json.dumps({"result": result}))


class state:
    """The state file, held under an exclusive lock and written back atomically.
    The tests take the same lock when they edit it."""

    def __init__(self):
        self.path = Path(os.environ["FAKE_HERDR_STATE"])

    def __enter__(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.lock = self.path.with_suffix(".lock").open("a+")
        fcntl.flock(self.lock, fcntl.LOCK_EX)
        self.current = json.loads(self.path.read_text()) if self.path.exists() else dict(EMPTY_STATE)
        return self.current

    def __exit__(self, exception_type, exception, traceback):
        if exception is None:
            temporary = self.path.with_suffix(".tmp")
            temporary.write_text(json.dumps(self.current, sort_keys=True))
            temporary.replace(self.path)
        fcntl.flock(self.lock, fcntl.LOCK_UN)
        self.lock.close()
        return False


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
