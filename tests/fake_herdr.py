#!/usr/bin/env python3
"""Herdr responses and Claude transcript entries for one idle implementer."""

import json
import os
import sys
from pathlib import Path


def main(argv):
    transcript = Path(os.environ["WORKER_TRANSCRIPT"])
    command = argv[:2]
    if command == ["tab", "create"]:
        return {"root_pane": {"pane_id": "pane-1"}, "tab": {"tab_id": "tab-1"}}
    if command == ["agent", "start"]:
        prompt = argv[-1]
    elif command == ["agent", "get"] and transcript.exists():
        return {"agent": {"status": "idle"}}
    elif command == ["agent", "prompt"] and transcript.exists():
        prompt = argv[3]
    else:
        sys.exit(f"unexpected herdr command: {argv!r}")

    with transcript.open("a") as log:
        print(json.dumps({"type": "user", "message": {"content": prompt}}), file=log)
    if command == ["agent", "start"]:
        return {"agent": {"agent_session": {"value": transcript.stem}, "status": "idle"}}
    return {"delivered": True}


if __name__ == "__main__":
    print(json.dumps({"result": main(sys.argv[1:])}))
