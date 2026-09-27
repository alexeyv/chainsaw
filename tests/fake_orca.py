#!/usr/bin/env python3
"""An `orca` standing in at the process boundary: the supervisor runs it like the
real CLI, and it answers with the JSON shapes and exit codes the real one produces.

Orca knows terminals by handle, not agents by name, so the agents in the JSON
file `FAKE_ORCA_STATE` names are keyed by the handle of the terminal each runs
in; the tests translate a session's name through the supervisor's own registry.
The tests read the state to see what the supervisor asked (`operations`) and
edit it to make an agent busy or drop prompts. Each agent's transcript is
written where that agent's real CLI would write it, in that CLI's format, so
the supervisor reads it back the same way it reads a real session's.

A real agent is mid-turn for a while after a prompt lands, and the supervisor's
wait looks for that; an agent here is busy for `fake_agent.TURN_SECONDS` after
each prompt it takes.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))

import json  # noqa: E402
import shlex  # noqa: E402
import time  # noqa: E402

import fake_agent as agents  # noqa: E402

EMPTY_STATE = {"agents": {}, "sequence": 0, "drop_prompts": 0, "operations": []}


def main(argv):
    handlers = {
        ("terminal", "create"): terminal_create,
        ("terminal", "split"): terminal_split,
        ("terminal", "wait"): terminal_wait,
        ("terminal", "send"): terminal_send,
    }
    handler = handlers.get(tuple(argv[:2]))
    if handler is None:
        print(f"unsupported: {' '.join(argv)}", file=sys.stderr)
        return 2
    return handler(agents.parse_flags(argv[2:]))


# --- the commands


def terminal_create(flags):
    title = flags.get("--title")
    with state() as current:
        handle, tab_id = open_terminal(current, flags["--command"], title=title)
    reply({"terminal": {"handle": handle, "tabId": tab_id, "title": title or ""}})
    return 0


def terminal_split(flags):
    with state() as current:
        handle, tab_id = open_terminal(
            current, flags["--command"], split_from=flags["--terminal"],
        )
    reply({"split": {"handle": handle, "tabId": tab_id}})
    return 0


def terminal_wait(flags):
    handle, timeout_ms = flags["--terminal"], int(flags["--timeout-ms"])
    with state() as current:
        agents.record(current, "wait", handle, timeout_ms=timeout_ms)
    deadline = time.monotonic() + timeout_ms / 1000
    while True:
        with state() as current:
            agent = current["agents"].get(handle)
            if agent is None:
                return refuse("terminal_handle_stale", f"no terminal {handle}")
            agents.drain_queue(current, agent)
            idle = not agents.is_busy(agent)
        if idle:
            reply({"wait": {"satisfied": True}})
            return 0
        if time.monotonic() >= deadline:
            return refuse("timeout", "timeout")
        time.sleep(0.01)


def terminal_send(flags):
    handle = flags["--terminal"]
    with state() as current:
        agent = current["agents"].get(handle)
        if "--interrupt" in flags:
            if agent is None:
                return refuse("terminal_handle_stale", f"no terminal {handle}")
            agents.end_turn(agent)
            agents.record(current, "interrupt", handle)
            reply({"send": {"accepted": True}})
            return 0
        text = flags["--text"]
        agents.record(current, "prompt", handle, text=text)
        if current["drop_prompts"] > 0:
            current["drop_prompts"] -= 1
            reply({"send": {"accepted": True}})
            return 0
        if agent is None:
            return refuse("terminal_handle_stale", f"no terminal {handle}")
        if "--enter" not in flags:
            # Typed but not submitted; the agent has nothing to pick up.
            reply({"send": {"accepted": True}})
            return 0
        if agents.is_busy(agent):
            agents.enqueue(agent, text)
        else:
            agents.drain_queue(current, agent)
            agents.deliver(current, agent, text)
            agents.begin_turn(agent)
    reply({"send": {"accepted": True}})
    return 0


# --- what a terminal runs


def open_terminal(current, command, title=None, split_from=None):
    current["sequence"] += 1
    number = current["sequence"]
    handle, tab_id = f"term-{number}", f"tab-{number}"
    run_dir, kind, session_id, args = launched(command)
    session_id = session_id or f"session-{title or handle}-{number}"
    agent = agents.new_agent(kind, run_dir, session_id)
    current["agents"][handle] = agent
    agents.open_transcript(agent)
    details = {"kind": kind, "args": args}
    if title is not None:
        details["title"] = title
    if split_from is not None:
        details["split_from"] = split_from
    agents.record(current, "start", handle, **details)
    return handle, tab_id


def launched(command):
    """The agent behind the shell command the supervisor hands a terminal:
    `cd RUN_DIR && exec AGENT [--session-id ID] ARGS...`."""
    words = shlex.split(command)
    if words[:1] != ["cd"] or words[2:4] != ["&&", "exec"] or len(words) < 5:
        raise SystemExit(f"fake orca cannot run {command!r}")
    run_dir, kind, args = words[1], words[4], words[5:]
    session_id = None
    if args[:1] == ["--session-id"]:
        session_id, args = args[1], args[2:]
    return run_dir, kind, session_id, args


# --- plumbing


def reply(result):
    print(json.dumps({"ok": True, "result": result}))


def refuse(code, message):
    """Orca reports a refusal in its JSON reply and exits non-zero."""
    print(json.dumps({"ok": False, "error": {"code": code, "message": message}}))
    return 1


def state():
    return agents.state("FAKE_ORCA_STATE", EMPTY_STATE)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
