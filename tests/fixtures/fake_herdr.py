#!/usr/bin/env python3
"""Small stateful Herdr fake used by the CLI contract tests.

The supervisor invokes Herdr as a subprocess, so the fake deliberately exposes the
same process boundary. Its state file is test-fixture state, not supervisor state.
"""

import fcntl
import json
import os
import sys
from pathlib import Path


STATE_PATH = Path(os.environ["FAKE_HERDR_STATE"])
LOCK_PATH = STATE_PATH.with_suffix(".lock")


def _load():
    try:
        return json.loads(STATE_PATH.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return {"agents": {}, "panes": {}, "sequence": 0, "drop_prompts": 0}


def _save(state):
    STATE_PATH.parent.mkdir(parents=True, exist_ok=True)
    temporary = STATE_PATH.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, sort_keys=True))
    temporary.replace(STATE_PATH)


def _option(args, name, default=None):
    try:
        return args[args.index(name) + 1]
    except (ValueError, IndexError):
        return default


def _logs_dir(run_dir):
    munged = os.path.realpath(run_dir).replace("/", "-").replace(".", "-")
    return Path(os.environ["HOME"]) / ".claude" / "projects" / munged


def _append(path, entry):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as stream:
        stream.write(json.dumps(entry, separators=(",", ":")) + "\n")


def _result(value):
    print(json.dumps(value, separators=(",", ":")))


def main():
    args = sys.argv[1:]
    LOCK_PATH.parent.mkdir(parents=True, exist_ok=True)
    with LOCK_PATH.open("a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        state = _load()

        if args[:2] == ["tab", "create"]:
            state["sequence"] += 1
            pane_id = f"pane-{state['sequence']}"
            tab_id = f"tab-{state['sequence']}"
            state["panes"][pane_id] = {"cwd": _option(args, "--cwd", os.getcwd())}
            _save(state)
            _result({"result": {"root_pane": {"pane_id": pane_id},
                                "tab": {"tab_id": tab_id}}})
            return

        if args[:2] == ["pane", "split"]:
            state["sequence"] += 1
            pane_id = f"pane-{state['sequence']}"
            state["panes"][pane_id] = {"cwd": _option(args, "--cwd", os.getcwd())}
            _save(state)
            _result({"result": {"pane": {"pane_id": pane_id}}})
            return

        if args[:2] == ["agent", "start"] and len(args) >= 3:
            name = args[2]
            pane_id = _option(args, "--pane")
            run_dir = state["panes"].get(pane_id, {}).get("cwd", os.getcwd())
            session_id = f"session-{name}-{state['sequence']}"
            state["agents"][name] = {
                "session_id": session_id,
                "status": "idle",
                "run_dir": run_dir,
            }
            _save(state)
            _result({"result": {"agent": {
                "agent_session": {"value": session_id},
                "status": "idle",
            }}})
            return

        if args[:2] == ["agent", "get"] and len(args) >= 3:
            agent = state["agents"].get(args[2])
            if not agent:
                sys.exit(1)
            _result({"result": {"agent": {
                "agent_session": {"value": agent["session_id"]},
                "status": agent.get("status", "idle"),
            }}})
            return

        if args[:2] == ["agent", "prompt"] and len(args) >= 4:
            name, prompt = args[2], args[3]
            agent = state["agents"].get(name)
            if not agent:
                sys.exit(1)
            if state.get("drop_prompts", 0) > 0:
                state["drop_prompts"] -= 1
                _save(state)
                _result({"result": {"delivered": False}})
                return
            log = _logs_dir(agent["run_dir"]) / f"{agent['session_id']}.jsonl"
            _append(log, {"type": "user", "message": {"content": prompt}})
            reply = state.get("reply_on_prompt")
            if reply:
                _append(log, {"type": "assistant", "message": {
                    "content": [{"type": "text", "text": reply}],
                }})
            _result({"result": {"delivered": True}})
            return

        if args[:2] == ["agent", "wait"]:
            _result({"result": {"status": "idle"}})
            return

        print(f"fake herdr: unsupported arguments: {args!r}", file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
