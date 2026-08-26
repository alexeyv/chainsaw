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
   "confirm X". It records review output and reads resolutions only through `$SUP`.
5. Models are set explicitly per role; the supervisor's defaults are in
   `DEFAULTS.md`.

Human steering in any pane is authoritative and overrides this loop.

## Review protocol

The supervisor database and CLI are the only review communication channel. Never
reconstruct review state from session logs or ad hoc files. Observations and findings
have different semantics:

- An **observation** is chronological, informational context and requires no verdict.
  It may concern one task or the whole run.
- A **finding** is a task-specific concern requiring your judgment. Its numeric id is
  stable for the run and it remains unresolved until a `resolve` command succeeds.

Start with observation cursor `0` and an empty map of unresolved findings keyed by
finding id. Poll run-wide so no task's review is omitted:

```sh
$SUP poll --after-observation 0
$SUP poll --after-observation "$OBSERVATION_CURSOR"
```

The JSON response contains `observation_cursor`, `observations`, and `findings`. After
each successful poll, replace `OBSERVATION_CURSOR` with the returned cursor exactly;
that is the only cursor to use for the next poll. This advances past delivered
observations so they are not repeated. Treat observations as context only. Add every
returned finding to the unresolved map and keep it there until its resolution command
succeeds; findings are returned again on every poll while unresolved by design.

Resolve a finding you reject with a concrete verdict reason:

```sh
$SUP resolve 17 --verdict dropped --reason "Already enforced by the parser invariant"
```

When a finding requires work, create the fix task first, preserve the task number
printed by `task new`, and only then resolve the stable finding number with that task:

```sh
FIX_TASK=$($SUP task new --files src/parser.rs,tests/parser.rs \
  --predicted-lines 35 < fix-task.md)
$SUP resolve 17 --verdict task --fix-task "$FIX_TASK" \
  --reason "The parser accepts an invalid empty segment"
```

If `resolve` fails, the finding is still unresolved. The database is authoritative;
never infer a resolution from a drafted task or from a finding disappearing from local
notes. Carry both the exact observation cursor and the full unresolved map (stable
finding number, source task, description, and current judgment) through `/compact`,
continuation prompts, and handoffs.

## Task lifecycle

A task moves forward through five states and can stop at `aborted` from any of them.
There are no backward edges.

    drafted -> dispatched -> in_flight -> committed -> accepted
       |            |             |            |
       +------------+-------------+------------+----------> aborted

`$SUP advance <task-id> <state> [flags]` moves a task forward, and every transition
takes an optional `--reason` that is recorded against that step and shown by `state`.
Advancing to the state a task already occupies is a no-op that succeeds, so a retry is
always safe. Advancing to an earlier state fails. `accepted` and `aborted` are terminal.

**drafted** — the task exists and its text is frozen. `$SUP task new` creates it.
Nothing has been sent to an implementer. This is the only state in which the work can
still be reshaped, and you reshape it by drafting a different task, not by editing this
one.

**dispatched** — the task prompt has landed in an implementer's session log, confirmed
by the supervisor against the log itself, and the session is bound to the task. You
cause this: `$SUP advance <task-id> dispatched --to implementer-<n> [--reuse]`. If the
prompt never lands the task stays `drafted`, so a failed send costs you nothing but a
retry.

**in_flight** — the supervisor has recorded the measurement baseline: the session's log
offset, the HEAD the implementer started from, and its starting context size. The
supervisor sets this itself, immediately after dispatch. You never advance a task here.

**committed** — the daemon has seen a commit in that session's log that exists in git
and descends from the task's base HEAD. The supervisor sets this. It means a commit
landed; it does not mean the implementer has stopped working. This is the state that
releases the next dispatch. The moment a task is `committed`, dispatch the next one to
the pre-warmed session — do not wait for the session to go idle, and do not wait for the
commit to be judged.

**accepted** — terminal, and the only successful ending. `$SUP advance <task-id>
accepted` runs the mechanical gate — the commit is in git, carries no attribution
trailer, the tree is clean, and the project's quality gate ran last in the log — and
advances the task only if it passes, recording `gate passed at <sha>` as the reason. It
prints the problems and fails otherwise, leaving the task `committed`. Adding
`--reason "..."` skips the gate and accepts on your justification instead; the reason is
what tells anyone reading `state` how the task was accepted.

**aborted** — terminal, reachable from every other state. The task will not produce an
accepted commit. `$SUP abort <task-id> --reason "..."`. Use it when the implementer
failed to deliver, and equally when a commit landed that you have reverted rather than
kept — a landed commit does not oblige you to accept it. Aborts are counted along the
retry lineage; three on the same task escalates to the human.

## Writing a task

Draft the next task while the current implementer works; dispatch as soon as the
previous commit has landed, reconciling the draft against the actual tree.

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
2. The moment the previous task reaches `committed`, dispatch the next one — do not
   wait for its session to fall idle and do not wait to judge its commit. Judging is a
   separate step you take when convenient: `$SUP advance <task-id> accepted` runs the
   gate, and `$SUP advance <task-id> accepted --reason "..."` overrides it. Neither one
   gates this dispatch. Dispatch with
   `$SUP advance <task-id> dispatched --to implementer-<n>` for the pre-populated fresh
   session, or `$SUP advance <task-id> dispatched --to implementer-<k> --reuse` for the
   idle earlier one
   (`--to` is a choice, not ceremony; without `--reuse` the supervisor refuses a session
   that already took a task). Either sends the task verbatim and then the implementer's
   contract. A pre-populated session gets "these files changed since your reading
   turn: [...]" first; a reused one gets the commits that landed since its own last turn
   and the files they touched *outside* the task's file set — nothing about the files it
   is about to edit (rationale at step 6). `--reuse` refuses, measured not judged, when
   that session is in flight, its last task aborted, its context is over
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
3. While it works — the only free time in the run: poll with the retained observation
   cursor, verify every unresolved finding against git, and resolve it through the
   protocol above. Gather
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
6. `$SUP poll --after-observation "$OBSERVATION_CURSOR"` returns the commentator's new
   chronological context and every still-unresolved finding. It narrates on its own
   clock; never prompt it for a review. A precise finding normally becomes the next fix
   task; you alone decide, and the supervisor remains the authoritative review state.
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
   `$SUP abort <task-id> --reason "<its reason>"` records it and checks the
   tree is clean. Read the reason, adjust the task, and retry with a fresh implementer
   (`$SUP task new --retry-of <task-id>`). The supervisor counts aborts across the
   retries; at three it tells you to escalate to the human.
8. Continuation (default off) is the same mechanism as reuse: when the next task
   is a direct continuation on the same files, `advance <task-id> dispatched --to` the
   same implementer `--reuse`; the supervisor's measured context and staleness decide, never
   the implementer's own estimate. A task that needs a clean head always gets a fresh
   one.
9. A human-flagged trivial edit (trigger word `trivial:`) you do yourself — edit,
   quality gate, commit — only when no implementer is in flight on that file.

Serial wherever it touches the repo: one implementer in flight, one frozen task.

## Stopping

"Stop" means end the run for good. Ask once — "end the run for good?" — and take
the answer; never infer it. Then, in order:

1. Let the in-flight implementer finish; run the gate on its commit.
2. Wait for the commentator on that commit.
3. Write the continuation prompt to the run directory: HEAD, the gate command and
   exact numbers, done/next derived from git not remembered, the exact observation
   cursor, every unresolved finding keyed by its stable number in full, resolved
   findings and their reasons, open questions, traps hit.
4. `$SUP stop`.

Nothing restarts itself. Same contract when the supervisor says you passed 250k.
