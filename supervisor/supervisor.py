#!/usr/bin/env python3
"""Chainsaw supervisor: the deterministic keeper of a run's state (guiding thought #49).

One program, two faces. `daemon` runs in the background and does the mechanical duties:
measure every session's context from its log, detect commits and commentator ingestion,
kick stale sessions, compact the commentator, tell the lead to stop at 250k. Every other
subcommand is the client the lead calls instead of driving Herdr by hand: launch an
implementer, prompt a role with verified delivery, record tasks and their state changes,
report state, append calibration and dispositions.

State lives in SQLite next to the run's session logs
(~/.claude/projects/<munged run dir>/chainsaw-supervisor.db), never in the run tree.

    --run-dir defaults to the current directory and goes before the subcommand.

    supervisor.py --run-dir DIR daemon --lead NAME
    supervisor.py --run-dir DIR start-commentator --role-prompt PATH
    supervisor.py --run-dir DIR launch NAME [--fresh --reason TEXT]
    supervisor.py --run-dir DIR prompt NAME TEXT [--wait]
    supervisor.py --run-dir DIR task new (--files a,b,c | --predicted-files N) --predicted-lines N [--retry-of ID] < task.md
    supervisor.py --run-dir DIR fail TASK --reason TEXT
    supervisor.py --run-dir DIR dispatch TASK --to NAME [--reuse]
    supervisor.py --run-dir DIR verify TASK
    supervisor.py --run-dir DIR accept TASK --reason TEXT
    supervisor.py --run-dir DIR calibrate TASK
    supervisor.py --run-dir DIR disposition FINDING --verdict task|dropped [--task ID] --reason TEXT
    supervisor.py --run-dir DIR config KEY [VALUE]
    supervisor.py --run-dir DIR state | comments [--all] | context [NAME] | human-wait start|end
    supervisor.py --run-dir DIR confirm-stop | stop
"""

import argparse
import fcntl
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import sys
import time

# ---- defaults (see DEFAULTS.md) -------------------------------------------------------

LEAD_STOP_TOKENS = 250_000
COMMENTATOR_COMPACT_TOKENS = 150_000
IMPLEMENTER_LIMIT_TOKENS = 100_000
STALE_SECONDS = 600
POLL_SECONDS = 5
# Reuse (#32): an idle implementer may take another task instead of a fresh launch when
# its measured context is under REUSE_MAX_CONTEXT and the tree has moved by at most
# REUSE_MAX_STALE_LINES changed lines (git diff --shortstat, insertions + deletions) since
# the commit it last saw. Both are overridable per run: config reuse-max-context N,
# config reuse-max-stale-lines N.
REUSE_MAX_CONTEXT = 60_000
REUSE_MAX_STALE_LINES = 200
PROMPT_ATTEMPTS = 3
PROMPT_TIMEOUT = 300
LEAN_FLAGS = [
    "--strict-mcp-config", "--no-chrome",
    "--disallowedTools",
    "WebSearch,WebFetch,NotebookEdit,Task,Agent,AskUserQuestion,EnterPlanMode,"
    "ExitPlanMode,TaskOutput",
]
IMPLEMENTER_FLAGS = ["--model", "opus", "--effort", "high",
                     "--disable-slash-commands", *LEAN_FLAGS]
COMMENTATOR_FLAGS = ["--model", "opus", "--effort", "high", *LEAN_FLAGS]
TRAILER_RE = re.compile(r"^(Co-Authored-By|Claude-Session):", re.M | re.I)
COMMIT_RE = re.compile(r"\[[\w/.-]+ ([0-9a-f]{7,40})\]")
TASK_STATES = ["drafted", "dispatched", "in_flight", "committed", "verified",
               "accepted", "ingested", "failed"]

CONTRACT = (
    "Verify the tree is clean; stop if dirty. Implement only this task. Run the task's "
    "checks, then the project's quality gate last. Commit without attribution trailers, "
    "leave the tree clean, then run exactly `git log -1 --format='[chainsaw %h]'` (the "
    "supervisor reads that record), and finish with the commit id, changed-file "
    "manifest, and a one-paragraph semantic delta."
)
NUDGE = "continue"

# ---- store ----------------------------------------------------------------------------

SCHEMA = """
create table if not exists config(key text primary key, value text);
create table if not exists tasks(id integer primary key, text text, predicted_files int,
  predicted_lines int, state text, implementer text, commit_sha text, created_at real,
  retry_of int, reason text, log_offset int default 0, base_head text,
  predicted_file_list text, reuse int default 0, context_base int);
create table if not exists task_log(task_id int, state text, at real);
create table if not exists sessions(name text primary key, role text, pane_id text,
  tab_id text, session_id text, started_at real, context int default 0,
  context_max int default 0, last_growth real, kicked_at real,
  prepopulated_at real, stopped_at real);
create table if not exists prompts(id integer primary key, session text, text text,
  sent_at real, landed_at real, attempts int);
create table if not exists calibration(task_id int primary key, predicted_files int,
  predicted_lines int, actual_files int, actual_lines int, wall_seconds real,
  context_tokens int, recorded_at real, context_base int, context_end int);
create table if not exists dispositions(id integer primary key, finding text,
  verdict text, task_id int, reason text, at real);
create table if not exists human_waits(id integer primary key, started real, ended real);
create table if not exists events(at real, kind text, detail text);
"""
# Columns added after a table first shipped; applied to databases created before them.
MIGRATIONS = [
    "alter table tasks add column predicted_file_list text",
    "alter table tasks add column reuse int default 0",
    "alter table tasks add column context_base int",
    "alter table calibration add column context_base int",
    "alter table calibration add column context_end int",
]


def logs_dir(run_dir):
    # Claude Code munges both "/" and "." to "-" when naming a project's
    # session-log directory (ui.wt -> ui-wt); diverging from that means
    # watching a directory the sessions never write to.
    munged = os.path.realpath(run_dir).replace("/", "-").replace(".", "-")
    return os.path.join(os.path.expanduser("~/.claude/projects"), munged)


class Store:
    def __init__(self, run_dir):
        self.run_dir = os.path.realpath(run_dir)
        self.logs = logs_dir(run_dir)
        os.makedirs(self.logs, exist_ok=True)
        self.path = os.path.join(self.logs, "chainsaw-supervisor.db")
        self.db = sqlite3.connect(self.path, timeout=30, isolation_level=None)
        self.db.row_factory = sqlite3.Row
        self.db.executescript(SCHEMA)
        for m in MIGRATIONS:
            try:
                self.db.execute(m)
            except sqlite3.OperationalError:
                pass  # column already there

    def q(self, sql, *a):
        return self.db.execute(sql, a).fetchall()

    def one(self, sql, *a):
        return self.db.execute(sql, a).fetchone()

    def event(self, kind, detail=""):
        self.db.execute("insert into events values(?,?,?)", (time.time(), kind, detail))

    def cfg(self, key, default=None):
        r = self.one("select value from config where key=?", key)
        return r["value"] if r else default

    def set_cfg(self, key, value):
        self.db.execute("insert or replace into config values(?,?)", (key, value))

    def set_task_state(self, task_id, state):
        assert state in TASK_STATES
        now = time.time()
        self.db.execute("update tasks set state=? where id=?", (state, task_id))
        self.db.execute("insert into task_log values(?,?,?)", (task_id, state, now))

    def session_log(self, name):
        """Path of the session's JSONL. Herdr is asked first — the id appears or changes
        when a session's first message lands — and the stored id is the fallback."""
        sid = herdr_session_id(name)
        if sid:
            self.db.execute("update sessions set session_id=? where name=?", (sid, name))
        else:
            s = self.one("select session_id from sessions where name=?", name)
            sid = s["session_id"] if s else None
        return os.path.join(self.logs, sid + ".jsonl") if sid else None


# ---- herdr ----------------------------------------------------------------------------

def herdr(*args, check=True):
    out = subprocess.run(["herdr", *args], check=check, capture_output=True, text=True)
    try:
        return json.loads(out.stdout)
    except json.JSONDecodeError:
        return {"raw": out.stdout}


def herdr_session_id(name):
    try:
        info = herdr("agent", "get", name)
    except subprocess.CalledProcessError:
        return None
    return (info["result"]["agent"].get("agent_session") or {}).get("value")


def herdr_status(name):
    try:
        return herdr("agent", "get", name)["result"]["agent"].get("status")
    except (subprocess.CalledProcessError, KeyError):
        return None


# ---- session log reading --------------------------------------------------------------

def iter_jsonl(path, offset=0):
    try:
        with open(path, errors="replace") as f:
            f.seek(offset)
            for line in f:
                try:
                    yield json.loads(line)
                except json.JSONDecodeError:
                    continue
    except OSError:
        return


def context_size(path):
    """Context fill in tokens, from the log's usage record — never self-report (#7).

    Same logic as tools/context-probe.cc: last 50 lines, scan backwards, skip sidechain
    entries, take the latest assistant/tool_result entry with a usage object and sum
    input + cache_read + cache_creation.
    """
    if not path:
        return 0
    try:
        out = subprocess.run(["tail", "-n", "50", path], capture_output=True, text=True)
    except OSError:
        return 0
    for line in reversed(out.stdout.splitlines()):
        u = usage_of_line(line)
        if u is not None:
            return u
    return 0


def usage_of_line(line):
    """Context fill recorded on one log line, or None when the line carries no usage."""
    if '"isSidechain":true' in line:
        return None
    if '"type":"assistant"' not in line and '"type":"tool_result"' not in line:
        return None
    i = line.find('"usage":{')
    if i < 0:
        return None
    seg = line[i:]

    def field(key):
        m = re.search(r'"%s":\s*(\d+)' % key, seg)
        return int(m.group(1)) if m else 0
    return (field("input_tokens") + field("cache_read_input_tokens")
            + field("cache_creation_input_tokens"))


def _log_lines(path, start=0, end=None):
    try:
        with open(path, "rb") as f:
            f.seek(start)
            data = f.read() if end is None else f.read(max(0, end - start))
    except (OSError, TypeError):
        return []
    return data.decode(errors="replace").splitlines()


def context_before(path, offset):
    """Context fill at byte `offset` of the log: the last usage recorded before it.

    The baseline a task starts from — what the session already carried when the task's
    prompt landed — so a reused session's calibration can subtract it (#48, #32).
    """
    last = 0
    for line in _log_lines(path, 0, offset):
        u = usage_of_line(line)
        if u is not None:
            last = u
    return last


def context_peak(path, start, end=None):
    """Largest context fill recorded in the log's byte window [start, end)."""
    peak = 0
    for line in _log_lines(path, start, end):
        u = usage_of_line(line)
        if u is not None and u > peak:
            peak = u
    return peak


def text_of(content):
    if isinstance(content, list):
        return " ".join(c.get("text", "") for c in content if isinstance(c, dict))
    return str(content)


def prompt_landed(path, offset, needle):
    for d in iter_jsonl(path, offset):
        if d.get("type") == "user" and needle in text_of(d.get("message", {}).get("content", "")):
            return True
    return False


def latest_assistant_text(path):
    last = None
    for d in iter_jsonl(path):
        if d.get("type") == "assistant":
            for c in d.get("message", {}).get("content", []):
                if isinstance(c, dict) and c.get("type") == "text" and c["text"].strip():
                    last = c["text"]
    return last


def bash_commands(path, offset=0):
    """Bash invocations in log order from offset, each paired with its result: (command, ok).

    ok is True only when a tool_result with the invocation's id arrived and is not an
    error; a command with no result, or one whose result says is_error / "Exit code N",
    is not ok.
    """
    calls, results = [], {}
    for d in iter_jsonl(path, offset):
        content = d.get("message", {}).get("content", [])
        if not isinstance(content, list):
            continue
        for c in content:
            if not isinstance(c, dict):
                continue
            if d.get("type") == "assistant" and c.get("type") == "tool_use" and c.get("name") == "Bash":
                calls.append((c.get("id"), c.get("input", {}).get("command", "")))
            elif c.get("type") == "tool_result":
                body = text_of(c.get("content", ""))
                ok = not c.get("is_error") and not re.match(r"\s*Exit code \d+", body)
                results[c.get("tool_use_id")] = ok
    return [(cmd, results.get(cid, False)) for cid, cmd in calls]


# Git plumbing that cannot change the tree the gate tested. `commit` is the
# endpoint, not plumbing; checkout/reset/rm/mv and friends are edits.
_GIT_PLUMBING = frozenset("""
    add status diff rev-parse log show ls-files cat-file ls-tree
    diff-tree rev-list name-rev symbolic-ref hash-object describe
    version help
""".split())
_READONLY_CMDS = frozenset("""
    echo printf true : ls pwd cat head tail wc test [ date cd
""".split())
_GITIGNORED_DIR = frozenset((
    "__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache",
    ".tox", ".coverage", ".eggs", ".cache",
))
_GITIGNORED_SUF = (".pyc", ".pyo", ".pyd", ".egg-info")
_PART_SPLIT = re.compile(r"(?:\n|&&|\|\||;|\|)+")
_FIND_WRITE = frozenset((
    "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fls", "-fprint", "-fprint0",
))


def _shell_parts(cmd):
    """Split a compound bash invocation. Heuristic, not a parser."""
    return [p.strip() for p in _PART_SPLIT.split(cmd.strip()) if p.strip()]


def _looks_gitignored(path):
    p = path.rstrip("/")
    name = os.path.basename(p)
    if name in _GITIGNORED_DIR or name.endswith(_GITIGNORED_SUF) or name.endswith("~"):
        return True
    return any("/" + d + "/" in "/" + p + "/" for d in _GITIGNORED_DIR)


def _is_safe_rm(part):
    """rm of gitignored/untracked-looking paths only; any other rm is an edit."""
    tokens = part.split()
    if not tokens or tokens[0] != "rm":
        return False
    paths, seen_dd = [], False
    for t in tokens[1:]:
        if not seen_dd and t == "--":
            seen_dd = True
            continue
        if not seen_dd and t.startswith("-"):
            continue
        paths.append(t)
    return bool(paths) and all(_looks_gitignored(p) for p in paths)


def _git_subcommand(tokens):
    """git [-C dir] [-c k=v] <subcommand> ... -> subcommand or None."""
    i = 1
    while i < len(tokens):
        t = tokens[i]
        if t in ("-C", "--git-dir", "--work-tree", "-c"):
            i += 2
            continue
        if t.startswith("-"):
            i += 1
            continue
        return t
    return None


def _harmless_after_gate(part, run_dir):
    """True when this shell part cannot change the tree the gate tested."""
    cleaned = re.sub(r"\d*>\s*/dev/null", "", part)
    run_root = os.path.realpath(run_dir) + os.sep
    for target in re.findall(r"\d*>>?\s*([^\s;|&]+)", cleaned):
        if target.startswith("&"):
            continue  # fd duplication, not a file write
        if os.path.isabs(target) and not (
                os.path.realpath(target) + os.sep).startswith(run_root):
            continue  # a write outside the run tree cannot dirty it
        return False
    tokens = part.split()
    while tokens and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", tokens[0]):
        tokens = tokens[1:]
    if not tokens:
        return True
    if tokens[0] == "mkdir":
        return True  # git does not track directories; an empty dir is invisible
    if tokens[0] == "git":
        return _git_subcommand(tokens) in _GIT_PLUMBING
    if tokens[0] == "find":
        return not any(t in _FIND_WRITE or t.startswith("-fprintf") for t in tokens)
    if tokens[0] in _READONLY_CMDS:
        return True
    return _is_safe_rm(part)


def gate_last_problem(cmds, gate, run_dir):
    """None if the gate ran, passed, and nothing after it could change its result.

    cmds is bash_commands() output: [(command, ok), ...]. Heuristic over the log
    (#35): git plumbing (add/status/diff/rev-parse) and rm of gitignored-looking
    paths may sit between the gate and `git commit`; an edit, a tracked-looking
    rm, or a failing command may not.
    """
    idx = max((i for i, (c, _) in enumerate(cmds) if "git commit" in c), default=-1)
    if idx < 0:
        return f"no git commit in the log; gate-last unchecked ({gate!r})"
    gate_idx = max((i for i, (c, _) in enumerate(cmds[:idx]) if gate in c), default=-1)
    if gate_idx < 0:
        return f"the gate ({gate!r}) did not run before the commit"
    if not cmds[gate_idx][1]:
        return "the gate before the commit failed (error result in the log)"
    intervening = list(cmds[gate_idx + 1:idx])
    before = re.split(r"\bgit\s+(?:-C\s+\S+\s+)?commit\b", cmds[idx][0], maxsplit=1)[0].strip()
    if before:
        intervening.append((before, True))
    for cmd, ok in intervening:
        if not ok:
            return f"a command after the gate failed: {cmd.splitlines()[0][:80]!r}"
        for part in _shell_parts(cmd):
            if not _harmless_after_gate(part, run_dir):
                return f"source-modifying command after the gate: {part[:80]!r}"
    return None


def commentator_agent_name(run_dir):
    """Herdr agent name unique to this run dir, stable across retries."""
    slug = hashlib.sha1(os.path.realpath(run_dir).encode()).hexdigest()[:8]
    return f"commentator-{slug}"


def commits_in_log(path, offset=0):
    try:
        with open(path, errors="replace") as f:
            f.seek(offset)
            return COMMIT_RE.findall(f.read())
    except OSError:
        return []


def fsize(path):
    try:
        return os.path.getsize(path)
    except (OSError, TypeError):
        return 0


def git(st, *args):
    return subprocess.run(["git", "-C", st.run_dir, *args], capture_output=True, text=True)


def new_commit_for(st, shas, base_head):
    """The last sha in the log that is in git, is not base_head, and descends from it."""
    for sha in reversed(shas):
        if base_head and base_head.startswith(sha):
            continue
        if git(st, "cat-file", "-e", sha).returncode:
            continue
        if base_head and git(st, "merge-base", "--is-ancestor", base_head, sha).returncode:
            continue
        return sha
    return None


# ---- reuse (#32) ----------------------------------------------------------------------

def cfg_int(st, key, default):
    try:
        return int(st.cfg(key, default))
    except (TypeError, ValueError):
        return default


def last_task_on(st, name):
    """The most recent task this session took (any state past drafted), or None."""
    return st.one("select * from tasks where implementer=? and state!='drafted' "
                  "order by id desc limit 1", name)


def last_seen_commit(st, name):
    """The commit whose tree the session's memory corresponds to: its own last commit, or
    the HEAD it was dispatched on if it never landed one. None if it never took a task."""
    t = last_task_on(st, name)
    if not t:
        return None
    return t["commit_sha"] or t["base_head"]


def staleness(st, since_sha):
    """How far the tree moved since a commit: (commits, files, lines) up to HEAD."""
    if not since_sha:
        return 0, 0, 0
    n = git(st, "rev-list", "--count", f"{since_sha}..HEAD").stdout.strip()
    stat = git(st, "diff", "--shortstat", since_sha, "HEAD").stdout
    files = int((re.search(r"(\d+) files? changed", stat) or [0, 0])[1])
    ins = int((re.search(r"(\d+) insertions?", stat) or [0, 0])[1])
    dels = int((re.search(r"(\d+) deletions?", stat) or [0, 0])[1])
    return int(n or 0), files, ins + dels


def files_changed_since(st, since_sha):
    if not since_sha:
        return []
    return git(st, "diff", "--name-only", since_sha, "HEAD").stdout.split()


def authored_files(st, name):
    """Files in the commits this session landed, in commit order, de-duplicated."""
    out, seen = [], set()
    for t in st.q("select commit_sha from tasks where implementer=? and commit_sha is not null "
                  "order by id", name):
        for f in git(st, "show", "--name-only", "--format=", t["commit_sha"]).stdout.split():
            if f not in seen:
                seen.add(f)
                out.append(f)
    return out


def reuse_verdict(st, sess):
    """None when the session may take another task, else why not (#32).

    Measured, not judged: idle (no task of its in flight), its last task did not fail,
    context under reuse-max-context, and the tree moved by at most reuse-max-stale-lines
    since the commit it last saw. In flight and too stale are the two refusals the
    dispatch --reuse path exists to make.
    """
    name = sess["name"]
    if sess["stopped_at"]:
        return "session is stopped"
    if st.one("select 1 from tasks where implementer=? and state in ('dispatched','in_flight')", name):
        return "session is in flight"
    last = last_task_on(st, name)
    if last and last["state"] == "failed":
        return f"its last task ({last['id']}) failed; a retry gets a fresh head"
    limit = cfg_int(st, "reuse-max-context", REUSE_MAX_CONTEXT)
    if sess["context"] > limit:
        return f"context {sess['context']} is over reuse-max-context {limit}"
    commits, files, lines = staleness(st, last_seen_commit(st, name))
    stale_limit = cfg_int(st, "reuse-max-stale-lines", REUSE_MAX_STALE_LINES)
    if lines > stale_limit:
        return (f"tree moved {lines} lines in {files} files over {commits} commits since its "
                f"last turn, over reuse-max-stale-lines {stale_limit}: its memory of the tree "
                f"is wrong, not merely old")
    return None


def idle_pool(st):
    """Implementers that could take the next task instead of a fresh launch, with their
    measured figures: [(session row, context, (commits, files, lines), authored files)]."""
    pool = []
    for s in st.q("select * from sessions where role='implementer' order by started_at"):
        if reuse_verdict(st, s):
            continue
        pool.append((s, s["context"], staleness(st, last_seen_commit(st, s["name"])),
                     authored_files(st, s["name"])))
    return pool


def describe_idle(st, s, ctx, stale, files):
    """One line naming the session, its measured figures, and the dispatch that uses it.

    A session that has never taken a task is idle but not a reuse: it takes a plain
    dispatch. Saying so here keeps the refusal's advice from contradicting dispatch's.
    """
    if not last_task_on(st, s["name"]):
        return (f"{s['name']} is idle at context {ctx} and has never taken a task — "
                f"dispatch <task-id> --to {s['name']}")
    commits, nfiles, lines = stale
    return (f"{s['name']} is idle at context {ctx}, tree moved {lines} lines/{nfiles} files/"
            f"{commits} commits since its last turn, authored "
            + (", ".join(files) if files else "nothing yet")
            + f" — dispatch <task-id> --to {s['name']} --reuse")


# ---- client commands ------------------------------------------------------------------

def cmd_launch(st, name, role="implementer", flags=None, split=False, fresh=False, reason=None):
    """Start a fresh session in its own Herdr tab (or a pane split from the caller).

    An implementer launch refuses while an idle implementer could take the next task
    (#32): the lead either dispatches to it with --reuse or passes --fresh --reason,
    which is recorded like accept's override. Refusing at the action, not advice in
    state: the idle session's figures were visible in state all along and changed
    nothing.
    """
    if role == "implementer":
        pool = [p for p in idle_pool(st) if p[0]["name"] != name]
        if pool and not fresh:
            print(f"supervisor: launch {name} refused — an idle implementer can take the next task:",
                  file=sys.stderr)
            for s, ctx, stale, files in pool:
                print("  " + describe_idle(st, s, ctx, stale, files), file=sys.stderr)
            sys.exit(f"dispatch to one of those, or launch {name} --fresh --reason \"...\" "
                     "to record why a fresh head is needed")
        if fresh:
            if not (reason or "").strip():
                sys.exit("supervisor: --fresh requires a non-empty --reason")
            idle = ", ".join(s["name"] for s, *_ in pool) or "none"
            st.event("launch-fresh", f"{name} (idle: {idle}): {reason}")
    workspace = os.environ.get("HERDR_WORKSPACE_ID")
    if not workspace:
        sys.exit("supervisor: must run inside a Herdr pane")
    if split:
        pane = herdr("pane", "split", "--current", "--direction", "right",
                     "--cwd", st.run_dir, "--no-focus")
        pane_id, tab_id = pane["result"]["pane"]["pane_id"], os.environ.get("HERDR_TAB_ID")
    else:
        tab = herdr("tab", "create", "--workspace", workspace, "--label", name,
                    "--cwd", st.run_dir, "--no-focus")
        pane_id, tab_id = tab["result"]["root_pane"]["pane_id"], tab["result"]["tab"]["tab_id"]
    for attempt in range(5):
        try:
            started = herdr("agent", "start", name, "--kind", "claude", "--pane", pane_id,
                            "--", *(flags or IMPLEMENTER_FLAGS))
            break
        except subprocess.CalledProcessError:
            if attempt == 4:
                raise
            time.sleep(2)
    sid = (started["result"]["agent"].get("agent_session") or {}).get("value")
    st.db.execute(
        "insert or replace into sessions(name,role,pane_id,tab_id,session_id,started_at,"
        "last_growth) values(?,?,?,?,?,?,?)",
        (name, role, pane_id, tab_id, sid, time.time(), time.time()))
    st.event("launch", name)
    print(json.dumps({"name": name, "pane_id": pane_id, "tab_id": tab_id, "session_id": sid}))


def cmd_prompt(st, name, text, wait=False, timeout=PROMPT_TIMEOUT):
    """Serial, delivery-verified prompt (#34, #35).

    Returns as soon as the prompt is seen as a user message in the role's session log —
    it does not wait for the turn, so the lead keeps its free time while an implementer
    works (#18, #30) and the daemon's polling is never suspended. With wait=True it also
    waits for the role to settle and prints its last reply.
    """
    lock = open(st.path + ".prompt-lock", "w")
    fcntl.flock(lock, fcntl.LOCK_EX)  # one submission at a time across clients and daemon
    needle = text[:80]
    pid = st.db.execute(
        "insert into prompts(session,text,sent_at,attempts) values(?,?,?,0)",
        (name, text, time.time())).lastrowid
    try:
        for attempt in range(PROMPT_ATTEMPTS):
            path_before = st.session_log(name)
            offset = fsize(path_before)
            st.db.execute("update prompts set attempts=attempts+1 where id=?", (pid,))
            subprocess.run(["herdr", "agent", "prompt", name, text],
                           check=False, capture_output=True, text=True)
            deadline = time.time() + 15
            while time.time() < deadline:
                path = st.session_log(name)
                if path != path_before:
                    offset = 0
                if path and prompt_landed(path, offset, needle):
                    st.db.execute("update prompts set landed_at=? where id=?", (time.time(), pid))
                    fcntl.flock(lock, fcntl.LOCK_UN)
                    if wait:
                        subprocess.run(["herdr", "agent", "wait", name, "--timeout",
                                        str(timeout * 1000)], check=False, capture_output=True)
                        print(latest_assistant_text(path) or "(no assistant text)")
                    return True
                time.sleep(1)
            print(f"prompt did not land (attempt {attempt + 1}), resending", file=sys.stderr)
        st.event("prompt-failed", name)
        sys.exit(f"supervisor: prompt to {name} never landed after {PROMPT_ATTEMPTS} attempts")
    finally:
        fcntl.flock(lock, fcntl.LOCK_UN)


def cmd_start_commentator(st, role_prompt):
    name = commentator_agent_name(st.run_dir)
    cmd_launch(st, name, role="commentator", flags=COMMENTATOR_FLAGS, split=True)
    st.set_cfg("commentator", name)
    cmd_prompt(st, name,
               f"Read and follow this role prompt entirely: {os.path.realpath(role_prompt)}\n"
               f"Session-log directory: {st.logs}\nRun directory: {st.run_dir}")


def cmd_task_new(st, predicted_files, predicted_lines, retry_of=None, files=None):
    """Record a drafted task. --files names the predicted file set (comma-separated);
    --predicted-files N is the count alone. With a list the supervisor can tell a reused
    implementer what moved outside its task's files and nothing about the files it will
    edit; with a count it cannot."""
    text = sys.stdin.read()
    if not text.strip():
        sys.exit("supervisor: task text on stdin is empty")
    if retry_of and not st.one("select 1 from tasks where id=? and state='failed'", retry_of):
        sys.exit(f"supervisor: --retry-of {retry_of} is not a failed task")
    file_list = [f.strip() for f in (files or "").split(",") if f.strip()]
    if file_list:
        if predicted_files is not None and predicted_files != len(file_list):
            sys.exit(f"supervisor: --predicted-files {predicted_files} disagrees with "
                     f"--files ({len(file_list)} names); give one or the other")
        predicted_files = len(file_list)
    elif predicted_files is None:
        sys.exit("supervisor: task new needs --files a,b,c or --predicted-files N")
    tid = st.db.execute(
        "insert into tasks(text,predicted_files,predicted_lines,state,created_at,retry_of,"
        "predicted_file_list) values(?,?,?,?,?,?,?)",
        (text, predicted_files, predicted_lines, "drafted", time.time(), retry_of,
         ",".join(file_list) or None)
    ).lastrowid
    st.set_task_state(tid, "drafted")
    print(tid)


def predecessor_unverified(st, task_id):
    """Message if the previous task is past in_flight but was never verified.

    Failed and still-drafted predecessors do not block (retry / not-yet-started).
    Verified or accepted (including those later ingested) do not block.
    """
    prev = st.one("select * from tasks where id < ? order by id desc limit 1", task_id)
    if not prev or prev["state"] in ("drafted", "dispatched", "in_flight", "failed"):
        return None
    seen = {r["state"] for r in st.q("select state from task_log where task_id=?", prev["id"])}
    if "verified" in seen or "accepted" in seen:
        return None
    return (f"task {prev['id']} is {prev['state']} but never verified; "
            f"run verify {prev['id']}, or accept {prev['id']} --reason ... "
            f"if the failure is a false positive")


def reuse_preamble(st, task, to):
    """What a reused implementer has no reason to open and so cannot discover by itself:
    the commits that landed since its last turn and the files they touched outside this
    task's own file set. Deliberately nothing about the files it is about to edit — a
    stale memory of those fails loudly on the first exact-match edit and it goes and
    reads; a stale memory of the rest never collides with anything (#32)."""
    since = last_seen_commit(st, to)
    commits, _, _ = staleness(st, since)
    if not commits:
        return ""
    log = git(st, "log", "--oneline", "--no-decorate", f"{since}..HEAD").stdout.strip()
    changed = files_changed_since(st, since)
    own = set((task["predicted_file_list"] or "").split(",")) - {""}
    outside = [f for f in changed if f not in own]
    lines = [f"Since your last turn (your commit {since[:10]}), {commits} commit"
             f"{'s' if commits != 1 else ''} landed:", log]
    if own:
        lines.append("Outside this task's files they touched: "
                     + (", ".join(outside) if outside else "nothing")
                     + ". Files in your task are not listed here; read them fresh as you open them.")
    else:
        lines.append("They touched: " + (", ".join(changed) or "nothing")
                     + " (this task carries no file list, so this is unfiltered).")
    return "\n".join(lines) + "\n\n"


def cmd_dispatch(st, task_id, to, reuse=False):
    task = st.one("select * from tasks where id=?", task_id)
    if not task or task["state"] != "drafted":
        sys.exit(f"supervisor: task {task_id} is not in state drafted")
    flying = st.one("select * from tasks where state in ('dispatched','in_flight')")
    if flying:
        who = f"{flying['implementer']} is in flight on task {flying['id']}"
        sys.exit(f"supervisor: an implementer is already in flight ({who})")
    blocked = predecessor_unverified(st, task_id)
    if blocked:
        sys.exit("supervisor: " + blocked)
    sess = st.one("select * from sessions where name=?", to)
    if not sess:
        sys.exit(f"supervisor: no session {to}; launch it first")
    prior = last_task_on(st, to)
    if prior and not reuse:
        sys.exit(f"supervisor: {to} already took task {prior['id']} ({prior['state']}); "
                 "dispatching to it again is a reuse: pass --reuse, or launch a "
                 "fresh implementer")
    if reuse and not prior:
        sys.exit(f"supervisor: {to} has never taken a task; dispatch without --reuse")
    if reuse:
        why = reuse_verdict(st, sess)
        if why:
            sys.exit(f"supervisor: cannot reuse {to}: {why}")
    preamble = ""
    if reuse:
        preamble = reuse_preamble(st, task, to)
    elif sess["prepopulated_at"]:
        since = st.one("select commit_sha from tasks where state in ('committed','verified',"
                       "'accepted','ingested') order by id desc limit 1")
        if since and since["commit_sha"]:
            files = subprocess.run(["git", "-C", st.run_dir, "show", "--name-only",
                                    "--format=", since["commit_sha"]],
                                   capture_output=True, text=True).stdout.split()
            preamble = ("These files changed since your reading turn; read them first: "
                        + ", ".join(files) + "\n\n")
    st.db.execute("update tasks set implementer=?, reuse=? where id=?", (to, int(reuse), task_id))
    st.set_task_state(task_id, "dispatched")
    st.event("dispatch", f"task {task_id} -> {to}")
    try:
        cmd_prompt(st, to, preamble + task["text"].rstrip() + "\n\n" + CONTRACT)
    except SystemExit:
        # Never landed: the task is not in flight; leave it dispatchable again.
        st.set_task_state(task_id, "drafted")
        st.event("dispatch-failed", f"task {task_id} -> {to}: prompt never landed")
        raise
    # Landed: the implementer is working. Remember where in its log this task starts,
    # what HEAD was, and what context the session already carried, so a reused
    # implementer's earlier commit is never taken for this one and its calibration
    # describes this task, not the session's cumulative total (#32, #48). The daemon marks
    # committed from the log; the lead is free to draft and pre-populate the next task
    # (#18, #30).
    log = st.session_log(to)
    offset = fsize(log)
    base = context_before(log, offset)
    st.db.execute("update tasks set log_offset=?, base_head=?, context_base=? where id=?",
                  (offset, git(st, "rev-parse", "HEAD").stdout.strip(), base, task_id))
    st.set_task_state(task_id, "in_flight")
    print(f"task {task_id} in flight on {to}" + (f" (reuse, context base {base})" if reuse else ""))
    # A session that took a dispatch is no longer pre-populated for a later one.
    st.db.execute("update sessions set prepopulated_at=NULL where name=?", (to,))


def cmd_verify(st, task_id):
    """Commit landed, no trailers, tree clean, gate last in the log (#35)."""
    task = st.one("select * from tasks where id=?", task_id)
    if not task:
        sys.exit(f"supervisor: no task {task_id}")
    log = st.session_log(task["implementer"]) if task["implementer"] else None
    shas = commits_in_log(log, task["log_offset"] or 0) if log else []
    sha = task["commit_sha"] or new_commit_for(st, shas, task["base_head"])
    problems = []
    if not sha:
        problems.append("no commit found in the implementer's log")
    else:
        show = subprocess.run(["git", "-C", st.run_dir, "log", "-1", "--format=%H%n%B", sha],
                              capture_output=True, text=True)
        if show.returncode:
            problems.append(f"commit {sha} not in git")
        elif TRAILER_RE.search(show.stdout):
            problems.append("commit carries an attribution trailer")
        head = subprocess.run(["git", "-C", st.run_dir, "rev-parse", "HEAD"],
                              capture_output=True, text=True).stdout.strip()
        if show.returncode == 0 and not head.startswith(show.stdout.split("\n", 1)[0]):
            problems.append("commit is not HEAD")
    if subprocess.run(["git", "-C", st.run_dir, "status", "--porcelain"],
                      capture_output=True, text=True).stdout.strip():
        problems.append("tree is dirty")
    gate = st.cfg("gate")
    if gate and log:
        cmds = bash_commands(log, task["log_offset"] or 0)
        problem = gate_last_problem(cmds, gate, st.run_dir)
        if problem:
            problems.append(problem)
    elif not gate:
        problems.append("gate not configured (supervisor.py config gate CMD): gate-last unchecked")
    if sha and not task["commit_sha"]:
        st.db.execute("update tasks set commit_sha=? where id=?", (sha, task_id))
    hard = [p for p in problems if not p.startswith("gate not configured")]
    if not hard:
        st.set_task_state(task_id, "verified")
        print(f"task {task_id} verified: {sha}")
        for p in problems:
            print("note:", p)
        return
    print(f"task {task_id} NOT verified:")
    for p in problems:
        print(" -", p)
    sys.exit(1)


def cmd_accept(st, task_id, reason):
    """Lead judged a verify failure a false positive; record it so dispatch may proceed.

    Not the default path: requires a reason, and the task must have a commit that was
    never verified. Visible in `state` as accepted (or as accepted@ in the timeline
    if the commentator already ingested it).
    """
    task = st.one("select * from tasks where id=?", task_id)
    if not task:
        sys.exit(f"supervisor: no task {task_id}")
    if not (reason or "").strip():
        sys.exit("supervisor: accept requires a non-empty --reason")
    seen = {r["state"] for r in st.q("select state from task_log where task_id=?", task_id)}
    if "verified" in seen:
        sys.exit(f"supervisor: task {task_id} is already verified")
    if "accepted" in seen:
        sys.exit(f"supervisor: task {task_id} is already accepted")
    if task["state"] not in ("committed", "ingested") or not task["commit_sha"]:
        sys.exit(f"supervisor: task {task_id} is {task['state']}, not a committed unverified task")
    st.db.execute("update tasks set reason=? where id=?", (reason, task_id))
    if task["state"] == "committed":
        st.set_task_state(task_id, "accepted")
    else:
        # Commentator already ingested it; record the override without rolling back.
        st.db.execute("insert into task_log values(?,?,?)", (task_id, "accepted", time.time()))
    st.event("accepted", f"task {task_id}: {reason}")
    print(f"task {task_id} accepted without verify: {reason}")


def failures_in_lineage(st, task_id):
    """Failed attempts on this task and the tasks it retries (#28)."""
    n, t = 0, st.one("select * from tasks where id=?", task_id)
    while t:
        n += t["state"] == "failed"
        t = st.one("select * from tasks where id=?", t["retry_of"]) if t["retry_of"] else None
    return n


def cmd_fail(st, task_id, reason):
    """The implementer reported it cannot land the task (#28)."""
    task = st.one("select * from tasks where id=?", task_id)
    if not task or task["state"] not in ("dispatched", "in_flight"):
        sys.exit(f"supervisor: task {task_id} is not in flight")
    dirty = subprocess.run(["git", "-C", st.run_dir, "status", "--porcelain"],
                           capture_output=True, text=True).stdout.strip()
    st.db.execute("update tasks set reason=? where id=?", (reason, task_id))
    st.set_task_state(task_id, "failed")
    st.event("failed", f"task {task_id}: {reason}")
    n = failures_in_lineage(st, task_id)
    print(f"task {task_id} failed ({n} failure{'s' if n != 1 else ''} on this task): {reason}")
    if dirty:
        print("WARNING: tree is dirty — the implementer did not leave it clean:\n" + dirty)
    if n >= 3:
        print("three failures on the same task: escalate to the human")
    else:
        print("adjust the task and retry with a fresh implementer: "
              f"task new --retry-of {task_id} < task.md")


def cmd_calibrate(st, task_id):
    """Predicted size vs actual size, wall time, and context (#48)."""
    task = st.one("select * from tasks where id=?", task_id)
    if not task or not task["commit_sha"]:
        sys.exit(f"supervisor: task {task_id} has no commit yet")
    stat = subprocess.run(["git", "-C", st.run_dir, "show", "--shortstat", "--format=",
                           task["commit_sha"]], capture_output=True, text=True).stdout
    files = int((re.search(r"(\d+) files? changed", stat) or [0, 0])[1])
    ins = int((re.search(r"(\d+) insertions?", stat) or [0, 0])[1])
    dels = int((re.search(r"(\d+) deletions?", stat) or [0, 0])[1])
    t0 = st.one("select at from task_log where task_id=? and state='dispatched'", task_id)
    t1 = st.one("select at from task_log where task_id=? and state='committed'", task_id)
    wall = (t1["at"] - t0["at"]) if t0 and t1 else None
    # Context is read from the session log's window for this task — from its dispatch to
    # the next dispatch on the same session, or the end of the log — and the baseline the
    # session carried at dispatch is subtracted, so a reused session's figure is this
    # task's cost, not the session's cumulative total (#32, #48).
    log = st.session_log(task["implementer"]) if task["implementer"] else None
    nxt = st.one("select log_offset from tasks where implementer=? and id>? and log_offset>0 "
                 "order by id limit 1", task["implementer"], task_id)
    end = context_peak(log, task["log_offset"] or 0, nxt["log_offset"] if nxt else None) if log else 0
    if not end:
        sess = st.one("select context_max from sessions where name=?", task["implementer"])
        end = sess["context_max"] if sess else 0
    base = task["context_base"] or 0
    ctx = max(end - base, 0)
    st.db.execute(
        "insert or replace into calibration(task_id,predicted_files,predicted_lines,"
        "actual_files,actual_lines,wall_seconds,context_tokens,recorded_at,context_base,"
        "context_end) values(?,?,?,?,?,?,?,?,?,?)",
        (task_id, task["predicted_files"], task["predicted_lines"], files, ins + dels,
         wall, ctx, time.time(), base, end))
    print(f"task {task_id}: predicted {task['predicted_files']} files/{task['predicted_lines']} "
          f"lines, actual {files} files/{ins + dels} lines, wall {wall and int(wall)}s, "
          f"context {ctx} (session {end}, base {base}"
          + (", reuse" if task["reuse"] else "") + ")")


def cmd_disposition(st, finding, verdict, task_id, reason):
    st.db.execute("insert into dispositions(finding,verdict,task_id,reason,at) values(?,?,?,?,?)",
                  (finding, verdict, task_id, reason, time.time()))
    write_dispositions_view(st)


def write_dispositions_view(st):
    """Text view for the commentator, generated from the table (#26, #49)."""
    lines = ["# Dispositions (generated by the supervisor; do not edit)", ""]
    for d in st.q("select * from dispositions order by id"):
        when = time.strftime("%Y-%m-%d %H:%M", time.localtime(d["at"]))
        tgt = f" (task {d['task_id']})" if d["task_id"] else ""
        lines.append(f"- {when} · {d['verdict']}{tgt} · {d['finding']} — {d['reason']}")
    path = os.path.join(st.logs, "chainsaw-dispositions.md")
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")


def cmd_comments(st, show_all=False):
    """New entries in the commentator's comments file since the lead last looked.

    The file is the commentator's own narration stream (chainsaw-comments.md next to the
    session logs); the supervisor only remembers how far the lead has read.
    """
    path = os.path.join(st.logs, "chainsaw-comments.md")
    try:
        text = open(path, errors="replace").read()
    except OSError:
        print("no comments file yet")
        return
    offset = 0 if show_all else int(st.cfg("comments-read", "0"))
    new = text[offset:]
    print(new if new.strip() else "no new comments")
    st.set_cfg("comments-read", str(len(text)))


def cmd_state(st):
    ts = lambda t: time.strftime("%H:%M:%S", time.localtime(t)) if t else "-"
    print("tasks")
    for t in st.q("select * from tasks order by id"):
        log = {r["state"]: r["at"] for r in st.q("select state, at from task_log where task_id=?", t["id"])}
        print(f"  {t['id']:>3} {t['state']:<10} {t['implementer'] or '-':<16} "
              f"{(t['commit_sha'] or '-')[:10]:<10} "
              + " ".join(f"{s}@{ts(log[s])}" for s in TASK_STATES if s in log)
              + (f"  retry of {t['retry_of']}" if t["retry_of"] else "")
              + (f"  reuse (context base {t['context_base']})" if t["reuse"] else "")
              + (f"  reason: {t['reason']}" if t["reason"] else ""))
    print("sessions")
    for s in st.q("select * from sessions order by started_at"):
        flag = ""
        if s["role"] == "implementer" and s["context"] > IMPLEMENTER_LIMIT_TOKENS:
            flag = " OVER-LIMIT"
        if s["prepopulated_at"]:
            flag += " pre-populated"
        if s["role"] == "implementer" and last_task_on(st, s["name"]) and not reuse_verdict(st, s):
            flag += " idle, reusable"
        print(f"  {s['name']:<16} {s['role']:<12} context {s['context']:>7} "
              f"(max {s['context_max']}) quiet {int(time.time() - (s['last_growth'] or time.time()))}s{flag}")
    first = st.one("select min(at) a from task_log")
    busy = 0.0
    for t in st.q("select id from tasks"):
        a = st.one("select at from task_log where task_id=? and state='dispatched'", t["id"])
        b = st.one("select at from task_log where task_id=? and state in ('committed','verified','ingested','failed') order by at limit 1", t["id"])
        if a:
            busy += ((b["at"] if b else time.time()) - a["at"])
    human = sum((w["ended"] or time.time()) - w["started"] for w in st.q("select * from human_waits"))
    if first and first["a"]:
        wall = time.time() - first["a"]
        print(f"time  wall {int(wall)}s  implementer-busy {int(busy)}s ({100*busy/wall:.0f}%)  "
              f"waiting-on-human {int(human)}s")
    fpath = os.path.join(st.logs, "chainsaw-comments.md")
    unread = fsize(fpath) - int(st.cfg("comments-read", "0"))
    if unread > 0:
        print(f"comments  {unread} bytes unread (supervisor.py comments)")
    open_wait = st.one("select 1 from human_waits where ended is null")
    if open_wait:
        print("  (a human wait is open)")
    if st.cfg("stop-confirmed") == "1":
        print("stop-confirmed  (human ran confirm-stop; stop is allowed)")
    for e in st.q("select * from events where kind in ('stop-lead','kick','compact','stop-confirmed','accepted','launch-fresh') order by at desc limit 5"):
        print(f"  {ts(e['at'])} {e['kind']} {e['detail']}")


def cmd_context(st, name=None):
    rows = st.q("select name from sessions where ? is null or name=? order by started_at", name, name)
    for r in rows:
        print(f"{r['name']}\t{context_size(st.session_log(r['name']))}")


def cmd_human_wait(st, action):
    if action == "start":
        if not st.one("select 1 from human_waits where ended is null"):
            st.db.execute("insert into human_waits(started) values(?)", (time.time(),))
    else:
        st.db.execute("update human_waits set ended=? where ended is null", (time.time(),))


def cmd_confirm_stop(st):
    """Human-only confirmation that the run may end. The lead must never call this.

    Refuses when invoked from the lead's Herdr pane so an auto-answered prompt
    cannot complete the gate. The check makes accidental self-confirmation
    impossible, not deliberate self-confirmation. `stop` (and the skill's stop
    sequence) wait on this.
    """
    pane = os.environ.get("HERDR_PANE_ID") or ""
    lead_pane = st.cfg("lead-pane") or ""
    if lead_pane and pane == lead_pane:
        sys.exit("supervisor: confirm-stop must be run by the human, not from the lead's pane")
    st.set_cfg("stop-confirmed", "1")
    st.event("stop-confirmed", "human confirmed the run may stop")
    print("supervisor: stop confirmed; the lead may run the stop sequence")


def cmd_stop(st):
    if st.cfg("stop-confirmed") != "1":
        sys.exit("supervisor: stop is not confirmed. The human must run: "
                 "supervisor.py --run-dir DIR confirm-stop")
    st.set_cfg("stopped", "1")
    st.event("stop", "run ended by the lead")
    print("supervisor: stopped; the daemon will exit on its next poll")


# ---- daemon ---------------------------------------------------------------------------

def daemon(st, lead):
    st.set_cfg("lead", lead)
    if os.environ.get("HERDR_PANE_ID"):
        st.set_cfg("lead-pane", os.environ["HERDR_PANE_ID"])
    st.db.execute("insert or ignore into sessions(name,role,started_at,last_growth) values(?,?,?,?)",
                  (lead, "lead", time.time(), time.time()))
    st.set_cfg("stopped", "0")
    st.event("daemon-start", f"pid {os.getpid()}")
    sizes = {}
    compacting = False
    while st.cfg("stopped") != "1":
        now = time.time()
        for s in st.q("select * from sessions where stopped_at is null"):
            name = s["name"]
            log = st.session_log(name)
            size = fsize(log)
            ctx = context_size(log)
            grew = size != sizes.get(name)
            sizes[name] = size
            st.db.execute(
                "update sessions set context=?, context_max=max(context_max,?), "
                "last_growth=case when ? then ? else last_growth end, "
                "kicked_at=case when ? then null else kicked_at end where name=?",
                (ctx, ctx, grew, now, grew, name))
            quiet = now - (s["last_growth"] or now)

            if s["role"] == "implementer":
                task = st.one("select * from tasks where implementer=? and state in "
                              "('dispatched','in_flight') order by id desc limit 1", name)
                if task:
                    shas = commits_in_log(log, task["log_offset"] or 0) if log else []
                    sha = new_commit_for(st, shas, task["base_head"]) if shas else None
                    if sha:
                        st.db.execute("update tasks set commit_sha=? where id=?", (sha, task["id"]))
                        st.set_task_state(task["id"], "committed")
                        st.event("committed", f"task {task['id']} {sha}")
                    elif quiet > STALE_SECONDS and not s["kicked_at"] and herdr_status(name) in ("idle", "done"):
                        st.event("kick", name)
                        st.db.execute("update sessions set kicked_at=? where name=?", (now, name))
                        cmd_prompt(st, name, NUDGE)
            elif s["role"] == "commentator":
                pending = st.q(
                    "select * from tasks where state in ('verified','committed','accepted') "
                    "and commit_sha is not null")
                if pending and log:
                    try:
                        text = open(log, errors="replace").read()
                    except OSError:
                        text = ""
                    for t in pending:
                        if t["commit_sha"][:7] in text:
                            st.set_task_state(t["id"], "ingested")
                            st.event("ingested", f"task {t['id']}")
                if ctx > COMMENTATOR_COMPACT_TOKENS and not compacting:
                    compacting = True
                    st.event("compact", f"{name} at {ctx}")
                    cmd_prompt(st, name, "/compact")
                elif ctx < COMMENTATOR_COMPACT_TOKENS:
                    compacting = False
                if quiet > STALE_SECONDS and not s["kicked_at"] \
                        and herdr_status(name) in ("idle", "done"):
                    st.event("kick", name)
                    st.db.execute("update sessions set kicked_at=? where name=?", (now, name))
                    cmd_prompt(st, name, NUDGE)
            elif s["role"] == "lead":
                if ctx > LEAD_STOP_TOKENS and st.cfg("lead-told-stop") != "1":
                    st.set_cfg("lead-told-stop", "1")
                    st.event("stop-lead", f"context {ctx}")
                    cmd_prompt(st, name,
                               f"supervisor: your context is {ctx} tokens, past {LEAD_STOP_TOKENS}. "
                               "Stop the run per the skill's Stopping section: the human must "
                               "run confirm-stop (you must never run it; do not ask a yes/no "
                               "question), then let the in-flight implementer finish, wait for "
                               "the commentator on that commit, write the continuation prompt.")
        time.sleep(POLL_SECONDS)
    st.event("daemon-exit", "")


# ---- main -----------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--run-dir", default=os.getcwd(),
                    help="the run's clean-slate checkout (default: the current directory); "
                         "goes before the subcommand")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("daemon"); p.add_argument("--lead", required=True)
    p = sub.add_parser("start-commentator"); p.add_argument("--role-prompt", required=True)
    p = sub.add_parser("launch"); p.add_argument("name")
    p.add_argument("--fresh", action="store_true",
                   help="launch even though an idle implementer could take the task")
    p.add_argument("--reason", help="why a fresh head is needed; required with --fresh, recorded")
    p = sub.add_parser("prompt"); p.add_argument("name"); p.add_argument("text")
    p.add_argument("--wait", action="store_true", help="also wait for the turn and print the reply")
    p.add_argument("--timeout", type=int, default=PROMPT_TIMEOUT)
    p.add_argument("--prepopulate", action="store_true",
                   help="mark the session as pre-populated (dispatch adds the changed-files preamble)")
    p = sub.add_parser("task"); p.add_argument("action", choices=["new"])
    p.add_argument("--files", help="predicted file set, comma-separated (preferred)")
    p.add_argument("--predicted-files", type=int, help="predicted file count, when no --files")
    p.add_argument("--predicted-lines", type=int, required=True)
    p.add_argument("--retry-of", type=int, help="the failed task this one retries")
    p = sub.add_parser("fail"); p.add_argument("task", type=int); p.add_argument("--reason", required=True)
    p = sub.add_parser("dispatch"); p.add_argument("task", type=int); p.add_argument("--to", required=True)
    p.add_argument("--reuse", action="store_true",
                   help="the target already took a task; measured refusals apply")
    p = sub.add_parser("verify"); p.add_argument("task", type=int)
    p = sub.add_parser("accept"); p.add_argument("task", type=int)
    p.add_argument("--reason", required=True)
    p = sub.add_parser("calibrate"); p.add_argument("task", type=int)
    p = sub.add_parser("disposition"); p.add_argument("finding")
    p.add_argument("--verdict", choices=["task", "dropped"], required=True)
    p.add_argument("--task", type=int); p.add_argument("--reason", required=True)
    p = sub.add_parser("config"); p.add_argument("key"); p.add_argument("value", nargs="?")
    sub.add_parser("state")
    p = sub.add_parser("comments"); p.add_argument("--all", action="store_true")
    p = sub.add_parser("context"); p.add_argument("name", nargs="?")
    p = sub.add_parser("human-wait"); p.add_argument("action", choices=["start", "end"])
    sub.add_parser("confirm-stop")
    sub.add_parser("stop")
    a = ap.parse_args()
    st = Store(a.run_dir)

    if a.cmd == "daemon":
        daemon(st, a.lead)
    elif a.cmd == "start-commentator":
        cmd_start_commentator(st, a.role_prompt)
    elif a.cmd == "launch":
        cmd_launch(st, a.name, fresh=a.fresh, reason=a.reason)
    elif a.cmd == "prompt":
        cmd_prompt(st, a.name, a.text, a.wait, a.timeout)
        if a.prepopulate:
            st.db.execute("update sessions set prepopulated_at=? where name=?", (time.time(), a.name))
    elif a.cmd == "task":
        cmd_task_new(st, a.predicted_files, a.predicted_lines, a.retry_of, a.files)
    elif a.cmd == "fail":
        cmd_fail(st, a.task, a.reason)
    elif a.cmd == "dispatch":
        cmd_dispatch(st, a.task, a.to, a.reuse)
    elif a.cmd == "verify":
        cmd_verify(st, a.task)
    elif a.cmd == "accept":
        cmd_accept(st, a.task, a.reason)
    elif a.cmd == "calibrate":
        cmd_calibrate(st, a.task)
    elif a.cmd == "disposition":
        cmd_disposition(st, a.finding, a.verdict, a.task, a.reason)
    elif a.cmd == "config":
        if a.value is None:
            print(st.cfg(a.key, ""))
        else:
            st.set_cfg(a.key, a.value)
    elif a.cmd == "state":
        cmd_state(st)
    elif a.cmd == "comments":
        cmd_comments(st, a.all)
    elif a.cmd == "context":
        cmd_context(st, a.name)
    elif a.cmd == "human-wait":
        cmd_human_wait(st, a.action)
    elif a.cmd == "confirm-stop":
        cmd_confirm_stop(st)
    elif a.cmd == "stop":
        cmd_stop(st)


if __name__ == "__main__":
    main()
