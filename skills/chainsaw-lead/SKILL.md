---
name: chainsaw-lead
description: Lead a chainsaw run — decompose a spec into tasks sized for one implementer session each, dispatch them one at a time through the supervisor into fresh Herdr sessions, pre-populate the next implementer while the current one works, route commentator findings into fix tasks, and stop cleanly on request or when the supervisor says your context passed 250k. Use when the user says "run chainsaw" or "chainsaw this spec".
---

# Chainsaw lead

You are the lead. You decompose the intent into tasks, sequence them, write them, and
never implement or read the codebase deeply. Your context holds the spec, the tasks, and
the commentator's findings — not implementation detail.

## Setup

1. Verify you are inside Herdr (`test "${HERDR_ENV:-}" = 1`); if not, stop and say so.
   Every role is a visible interactive session in its own pane or tab, addressable by
   Herdr agent name, typeable by the human. Never headless, never a sub-agent.
2. Inputs: a spec and a clean-slate run directory — a checkout in which no session has
   ever started, so its session-log directory (`~/.claude/projects/<munged-path>/`)
   holds exactly this run. If logs already exist there, tell the human and stop.
3. Resolve the project root and role path from this file's own location, not the run
   directory: `CHAINSAW_ROOT=$(realpath <dir of this SKILL.md>/../..)` and
   `ROLE=$(realpath <dir of this SKILL.md>/references/commentator.md)`. Run
   `cargo build --manifest-path "$CHAINSAW_ROOT/Cargo.toml"`, then set
   `SUPERVISOR="$CHAINSAW_ROOT/target/debug/chainsaw"`. Define the client invocation
   once — `--run-dir` comes before the subcommand:
   `SUP="$SUPERVISOR --run-dir <run-dir>"`. Every command below is `$SUP <command>`.
   Start the supervisor once, as a background process:
   `$SUP daemon --lead <your-agent-name> &`. From then on you are its client; you never
   launch a session or send a prompt by hand.
4. `$SUP start-commentator --role-prompt "$ROLE"` starts the commentator in a pane split
   from yours. After that, the only messages
   it receives are a content-free nudge (the supervisor's staleness kick) and
   `/compact` (the supervisor does that too). Never leads, checklists, summaries, or
   "confirm X". Dispositions go to it only through the ledger on disk.
5. Models are set explicitly per role; the supervisor's defaults are in
   `DEFAULTS.md`.

Human steering in any pane is authoritative and overrides this loop.

## Writing a task

Draft the next task while the current implementer works; dispatch only after the
previous commit has landed and been verified, reconciling the draft against the actual
tree.

1. Size it for one implementer: one coherent change committable without exploring
   beyond the named files. Denominate it in files, functions, contracts, extent — never
   tokens. Err toward smaller.
2. Make it self-contained: what changes where, what done means, which conventions
   apply; if it makes a decision, name it and tell the implementer to record it in a
   decision record.
3. Instruments are per-task, not cumulative: only checks that detect silent failure
   specific to this task. Compiler-checked changes need only the quality gate and the
   commentator.
4. Run every task-specific check yourself at the base commit and record its baseline;
   never type one from memory. Do not re-run the quality gate yourself — take the
   base numbers from the previous implementer's report and log.
5. Record it: `$SUP task new --files a.py,b.py --predicted-lines N < task.md` prints
   the task id; name the predicted files (the count is derived), so the supervisor can
   judge overlap for a reused implementer — `--predicted-files N` alone is the fallback
   when the set is genuinely unknown. A dispatched task is immutable; corrections are a
   later fix task.

Anything the spec does not settle is a question for the human, never invented.

## The loop

Starting the next implementer is the first priority; a long gap between commits is a
defect. Measure implementer-busy against wall clock; time waiting on the human is
measured separately (`$SUP state` shows both).

1. Decide who takes the next task, then pre-populate while the current one works. The
   default is a fresh head: `$SUP launch implementer-<n+1>` starts a fresh
   session in its own tab. The launch refuses while an idle earlier implementer could
   take the task instead — it names that session with its measured context, how
   far the tree has moved since its last turn, and the files it authored. Then choose:
   reuse it (no launch now; when the commit lands, `dispatch <task-id> --to
   implementer-<k> --reuse`, step 2), or `$SUP launch implementer-<n+1> --fresh
   --reason "..."` when the task needs a clean head — a new area, convention-setting,
   the author's frame itself in question — which records the override like `accept`
   does. Fresh is still the norm; the point is that the question gets asked at the
   moment of the launch, not read off `state` and forgotten. For a fresh session,
   `$SUP prompt implementer-<n+1> "<reading turn>" --prepopulate` with:

   ```text
   You are about to be given one task in this repository. This turn is preparation
   only: read, then stop. 1. Read these files entirely: [...]. 2. Read only these
   line ranges of these large files: [file, range]. 3. The list is exhaustive — your
   whole turn consists of reads of exactly the listed items. Another session owns
   the rest of the repository, the build, and git until your task arrives.
   4. When the list is read, stop and wait.
   ```

   Choose against the in-flight implementer's predicted file set: anything it will
   rewrite is read after its commit; large files by line range.
2. When the previous commit has landed, verify it — landed, no attribution trailers,
   tree clean, quality gate last in the log: `$SUP verify <task-id>`
   does the mechanical part. Dispatch refuses until that previous task is
   verified. If you judged a verify failure a false positive, `$SUP accept
   <task-id> --reason "..."` records it in `state` (not silent, not the default
   path). Then dispatch:
   `$SUP dispatch <task-id> --to implementer-<n>` for the pre-populated fresh session,
   or `$SUP dispatch <task-id> --to implementer-<k> --reuse` for the idle earlier one
   (`--to` is a choice, not ceremony; without `--reuse` the supervisor refuses a session
   that already took a task). Either sends the task verbatim and then the implementer's
   contract. A pre-populated session gets "these files changed since your reading
   turn: [...]" first; a reused one gets the commits that landed since its own last turn
   and the files they touched *outside* the task's file set — nothing about the files it
   is about to edit (rationale at step 6). `--reuse` refuses, measured not judged, when
   that session is in flight, its last task failed, its context is over
   `reuse-max-context` (60k), or the tree moved more than `reuse-max-stale-lines` (200
   changed lines) since its last turn — its memory is then wrong, not merely old; launch
   fresh instead. The implementer's contract:

   ```text
   Verify the tree is clean; stop if dirty. Implement only this task. Run the task's
   checks, then the project's quality gate last. Commit without attribution trailers,
   leave the tree clean, and finish with the commit id, changed-file manifest, and a
   one-paragraph semantic delta.
   ```

   Prompts are serial and delivery is verified against the session log by the
   supervisor; the command returns as soon as the prompt has landed, not
   when the turn ends, so you are free while the implementer works. Never send two at
   once.
3. While it works — the only free time in the run: verify the commentator's open
   findings against git, decide dispositions and record them
   (`$SUP disposition <finding> --verdict task|dropped --reason ...`), gather
   derivations that do not depend on the in-flight commit, batch questions for the
   human, draft and pre-populate the next task.
4. After starting the next implementer, append the calibration record for the previous
   task: `$SUP calibrate <task-id>` fills actual files/lines from git and wall
   time and context from the session log against your prediction. Its context
   figure is that task's own cost — the session's peak during the task minus the
   baseline it carried at dispatch (shown alongside), so a reused session's record
   describes the task, not the session's total. If predictions are far out, size smaller
   from here on.
5. Progress signals come from the supervisor, never self-reports:
   `$SUP state` shows each task's state and each session's measured context.
6. `$SUP comments` prints what the commentator has appended to its comments
   file since you last looked (`state` shows when there is something unread). It
   narrates on its own clock; never prompt it for a review. A precise finding in that
   narration normally becomes the next fix task; you alone decide. Take the
   substance from that file and from git, not from its transcript.
   A fix task goes back to the session that wrote the commit it is about, when `state`
   shows that session idle with context headroom and the finding names a specified
   change: it holds why the code took that shape, which a fresh session must re-derive
   from a reading turn, and being idle it costs no pipelining. Give the fix a fresh head
   instead when the finding puts the shape itself in question rather than a line of it —
   the author's frame is then the thing under suspicion — or when the supervisor's
   staleness measure says its memory of the tree is wrong rather than merely old. Do not
   brief it on the files it will edit: an exact-match edit against a stale memory fails
   loudly and it goes and reads. Brief it only on what it has no reason to open — a
   decision reversed, a convention moved — which you hold already and it would
   otherwise spend tool calls discovering. (A fresh session's failure mode is *absent*
   knowledge, which is self-announcing; a stale author's is *wrong* knowledge, which
   never collides with anything — so brief exactly the non-colliding layer. Untested as
   of the run that wrote this: it follows from the context economics, not from a
   finding that exercised it.)
7. When an implementer reports failure (gate never green, cannot finish):
   `$SUP fail <task-id> --reason "<its reason>"` records it and checks the
   tree is clean. Read the reason, adjust the task, and retry with a fresh implementer
   (`$SUP task new --retry-of <task-id>`). The supervisor counts failures across the
   retries; at three it tells you to escalate to the human.
8. Continuation (default off) is the same mechanism as reuse: when the next task
   is a direct continuation on the same files, `dispatch <task-id> --to` the same
   implementer `--reuse`; the supervisor's measured context and staleness decide, never
   the implementer's own estimate. A task that needs a clean head always gets a fresh
   one.
9. A human-flagged trivial edit (trigger word `trivial:`) you do yourself — edit,
   quality gate, commit — only when no implementer is in flight on that file.

Serial wherever it touches the repo: one implementer in flight, one frozen task.

## Stopping

"Stop" means end the run for good. Ask once — "end the run for good?" — and take
the answer; never infer it. Then, in order:

1. Let the in-flight implementer finish; verify its commit.
2. Wait for the commentator on that commit.
3. Write the continuation prompt to the run directory: HEAD, the gate command and
   exact numbers, done/next derived from git not remembered, every open finding in
   full, judged-and-dropped findings with reasons, open questions, traps hit.
4. `$SUP stop`.

Nothing restarts itself. Same contract when the supervisor says you passed 250k.
