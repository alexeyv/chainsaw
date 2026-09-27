#!/usr/bin/env python3
"""A `herdr` standing in at the process boundary: the supervisor runs it like the
real CLI, and it answers with the JSON shapes and exit codes the real one produces.

Its agents live in the JSON file `FAKE_HERDR_STATE` names, keyed by the name
Herdr knows them under. The tests read it to see what the supervisor asked
(`operations`) and edit it to make an agent busy or drop prompts. Each agent's
transcript is written where that agent's real CLI would write it, in that CLI's
format, so the supervisor reads it back the same way it reads a real session's.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))

import json  # noqa: E402
import time  # noqa: E402

import fake_agent as agents  # noqa: E402

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
    flags = agents.parse_flags(arguments)
    with state() as current:
        pane_id, tab_id = new_pane(current, flags["--cwd"])
    reply({"root_pane": {"pane_id": pane_id}, "tab": {"tab_id": tab_id}})
    return 0


def pane_split(arguments):
    flags = agents.parse_flags(arguments)
    with state() as current:
        pane_id, _ = new_pane(current, flags["--cwd"])
    reply({"pane": {"pane_id": pane_id}})
    return 0


def agent_start(arguments):
    name, flags = arguments[0], agents.parse_flags(arguments[1:])
    with state() as current:
        current["sequence"] += 1
        session_id = f"session-{name}-{current['sequence']}"
        run_dir = current["panes"][flags["--pane"]]["cwd"]
        kind = flags["--kind"]
        agent = agents.new_agent(kind, run_dir, session_id)
        current["agents"][name] = agent
        agents.open_transcript(current, agent, flags.get("--", []))
        agents.record(current, "start", name, kind=kind, args=flags["--"])
    reply({"agent": {"agent_session": {"value": session_id}, "status": "idle"}})
    return 0


def agent_get(arguments):
    name = arguments[0]
    with state() as current:
        agent = current["agents"].get(name)
        agents.drain_queue(current, agent)
        agents.record(current, "query", name)
    if agent is None:
        print("no such agent", file=sys.stderr)
        return 1
    status = "busy" if agents.is_busy(agent) else "idle"
    reply({"agent": {"agent_session": {"value": agent["session_id"]}, "status": status}})
    return 0


def agent_prompt(arguments):
    name, text = arguments[0], arguments[1]
    with state() as current:
        agents.record(current, "prompt", name, text=text)
        if current["drop_prompts"] > 0:
            current["drop_prompts"] -= 1
            reply({"delivered": True})
            return 0
        agent = current["agents"].get(name)
        if agent is None:
            print("no such agent", file=sys.stderr)
            return 1
        if agents.is_busy(agent):
            agents.enqueue(agent, text)
        else:
            agents.drain_queue(current, agent)
            agents.deliver(current, agent, text)
    reply({"delivered": True})
    return 0


def agent_send_keys(arguments):
    name = arguments[0]
    with state() as current:
        agent = current["agents"].get(name)
        if agent is None:
            print("no such agent", file=sys.stderr)
            return 1
        agents.end_turn(agent)
        agents.record(current, "interrupt", name)
    reply({"delivered": True})
    return 0


def agent_wait(arguments):
    name, flags = arguments[0], agents.parse_flags(arguments[1:])
    timeout = int(flags["--timeout"]) / 1000
    with state() as current:
        agents.record(current, "wait", name, timeout_ms=int(flags["--timeout"]))
    deadline = time.monotonic() + timeout
    while True:
        with state() as current:
            agent = current["agents"].get(name)
            agents.drain_queue(current, agent)
            idle = agent is None or not agents.is_busy(agent)
        if idle or time.monotonic() >= deadline:
            break
        time.sleep(0.01)
    reply({"status": "idle" if idle else "busy"})
    return 0


# --- plumbing


def new_pane(current, cwd):
    number = len(current["panes"]) + 1
    pane_id, tab_id = f"pane-{number}", f"tab-{number}"
    current["panes"][pane_id] = {"cwd": cwd}
    return pane_id, tab_id


def reply(result):
    print(json.dumps({"result": result}))


def state():
    return agents.state("FAKE_HERDR_STATE", EMPTY_STATE)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
