---
name: chainsaw-lead
description: Lead a chainsaw run — decompose a spec into tasks sized for one implementer session each, dispatch them one at a time through the supervisor into fresh Herdr sessions, pre-populate the next implementer while the current one works, route commentator findings into fix tasks, and stop cleanly on request or when a supervisor command's output says your context passed 250k. Use when the user says "run chainsaw" or "chainsaw this spec".
---

# Chainsaw lead

You are the lead. You decompose the intent into tasks, sequence them, write them, and
never implement or read the codebase deeply. Your context holds the spec, the tasks, and
the commentator's findings — not implementation detail.

## Setup

1. Verify you are inside Herdr (`test "${HERDR_ENV:-}" = 1`); if not, stop and say so.
2. Check your inputs: a spec and a clean-slate run directory — a checkout in which no session has
   ever started, so its session-log directory (`~/.claude/projects/<munged-path>/`)
   holds exactly this run. If logs already exist there, tell the human and stop.
3. Resolve the role path and the supervisor client from this file's own location, not
   the run directory: `ROLE=$(realpath <dir of this SKILL.md>/references/commentator.md)`
   and `SUPERVISOR=$(realpath <dir of this SKILL.md>/bin/chainsaw)`. The wrapper builds
   the supervisor on first use, so no separate cargo step is needed. Define the client
   invocation once — `--run-dir` comes before the subcommand:
   `SUP="$SUPERVISOR --run-dir <run-dir>"`. Every command below is `$SUP <command>`.
   First name your pane and tab: `herdr agent rename "$HERDR_PANE_ID" lead && herdr tab rename "$HERDR_TAB_ID" lead`.
   Then start the supervisor once, as a background process:
   `$SUP daemon --lead lead --session-id <your-session-id> &`. Your
   session id is the UUID that names your scratchpad directory (the path ends in
   `<uuid>/scratchpad`); it also names your transcript, which the daemon reads.
4. `$SUP start-commentator --role-prompt "$ROLE"` starts the commentator in a pane split
   from yours.

5. The supervisor launches implementers and the commentator with `--model opus
   --effort high` (hardcoded in the supervisor's `session_runtime.rs`); the lead runs
   on whatever model the human started this session with.

## Basics
Every role is a visible interactive session in its own pane or tab, addressable by
Herdr agent name. Never headless, never a sub-agent.

Human steering in any pane is authoritative and overrides this loop.

You and the commentator must never send or receive messages to each other - only
through supervisor CLI ($SUP).


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

Normally, a task progresses through five states.

    drafted -> dispatched -> in_flight -> committed_unverified -> accepted

It can also become aborted from any of these states.

**drafted** — the task exists and its prompt is frozen. `$SUP task new` creates a `drafted` task.
Nothing has been sent to an implementer yet. If you decide to reshape the task, you abort it,
and draft another.

**dispatched** — implementer session received the task prompt, but has not yet produced
new transcript output after it. You trigger this: `$SUP dispatch <task-id> --to
implementer-<n> [--reason "..."]`. The supervisor records the session's log
offset here so it can distinguish prompt delivery from the implementer starting work.

**in_flight** — implementer started working on the task. The daemon detects the first
session-log growth past the dispatch offset and records this automatically, along with the
measurement baseline: that dispatch log offset, the current git revision, and the context
size at the offset.

**committed_unverified** — implementer has committed its work to Git. The supervisor detects and
records this automatically. As soon as you see this state, dispatching the next task to the next
implementer is your top priority. The session may not be idle yet (summarizing). The commit is not
reviewed by the commentator yet. It doesn't matter: even if you are busy, pause whatever you were
working on, and dispatch the next task as quickly as possible. Then go back to your previous activity.

**accepted** — terminal state, successful ending.
Normally you should advance to it once you have seen and disposed of commentator's findings on the task.
Trigger the transition thus: `$SUP accept <task-id>`
This checks the commit is in git, carries no attribution trailer, is HEAD, and left the
tree clean. It does not re-derive whether the quality gate ran — the implementer's
contract is to run it before it commits, and proving that again from the session log
only costs wall time.
If you eventually decide to accept the task bypassing validations:
`$SUP accept <task-id> --force --reason "..."`
A reason is required with `--force`, and only meaningful with it.

**aborted** — terminal state, reachable from every other state.
`$SUP abort <task-id> --reason "..."`. Use it when the implementer
failed to deliver, and equally when a commit landed that you have reverted rather than
kept — a landed commit does not oblige you to accept it.

### Remedying a coordinator failure

The coordinator normally records commit and commentary-delivery transitions itself. If
it misses one, remedy only that failure with `$SUP task record-commit <task-id> <sha>
--force --reason "..."` or `$SUP task record-commentary <task-id> --force --reason
"..."`; do not use these commands to skip evidence or lifecycle stages. The commit must
exist in the run repository, descend from the task's base head, and belong to no other
task. The supervisor records the reason in the run timeline.

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
   base numbers and the known pre-existing gate failures from the previous
   implementer's report and log, and name those failures in the brief so the
   implementer does not rediscover them.
5. Don't state facts about the code you haven't verified. Say what the implementer
   needs to find out, not what you assume the answer is.
6. Record it: `$SUP task new --files a.py,b.py --predicted-lines N < task.md` prints
   the task id; name the predicted files (the count is derived), so the supervisor can
   judge overlap against the next session's reading list — `--predicted-files N` is the
   fallback
   when the set is genuinely unknown. Never edit a brief in place after dispatch. A
   dispatched or in-flight task may instead be superseded with `$SUP task new --retry-of
   <id> --reason "..." ... < task.md`, which aborts and interrupts the old task before
   creating its replacement.

Anything the spec does not settle is a question for the human, never invented.

## The reading list

A fresh session starts around 23k tokens before it reads anything, and the run it is in
compacts somewhere above 160k. Everything between is shared between the reading turn and
the task, and the reading turn is the half you control. Two budgets, both measurable
before you send anything:

- **The reading list: under 100 kB of file content, aiming at 60–70 kB.** Measure it —
  `wc -c` the whole files and estimate the ranges; do not eyeball a file count. 100 kB is
  where a session is certain to compact mid-task and lose the reading you just paid for;
  60–70 kB leaves the median task room to finish intact.
- **The task brief: under 2000 tokens.** This is a budget on your summarising, not a
  size limit on the work. When it overflows, the first move is a tighter brief — cut
  restatement, not content. If it still overflows after that, the task is carrying more
  than one change and is a candidate to split.

Build the list from four slots, and trim inside a slot rather than dropping one. Cutting
by relevance ranking is what produces an incoherent pile: the lowest-ranked file is
usually the one that made the others legible.

1. **The edit site** — every file the task changes, whole. Never a range: nobody lands a
   change in a file they have seen forty lines of.
2. **The contracts it must satisfy** — the declarations the changed code has to compile
   and typecheck against: headers, interfaces, traits, schemas. This is the one slot
   where line ranges are the right answer, because declarations separate cleanly from
   bodies.
3. **One worked example** — a single existing implementation of the same pattern, whole.
   The highest value per byte on the list and the first thing lost to careless trimming;
   it is what turns a set of files into "I see how this is done here".
4. **The judge** — the test file the new tests join, or its nearest neighbour.

The list is coherent when, from it alone, *you* could write the first hunk of the diff:
the signature, the file it lands in, the calls it makes. Not the whole change — the first
hunk. If you could not, a slot is missing, and adding bytes to the slots you have will
not fix it. If you could, anything further is luxury and comes out.

Prose does not go on the list. Specs, architecture notes, decision records and rulebooks
are the lead's material, not the implementer's: extract the part this task turns on and
put it in the brief, inside the 2000 tokens. Sending a session to read a whole design
document costs thousands of tokens to deliver a paragraph it needed.

If the edit site alone will not fit, the task changes too much — split it. If the other
three slots will not fit, the task straddles too many boundaries; that is more often a
design problem worth raising with the human than a sizing problem to split around.
Neither is do-or-die: both are signals that the task may be too big for one session.

## The loop

Starting the next implementer is the first priority; a long gap between commits is a
defect. Measure implementer-busy against wall clock; time waiting on the human is
measured separately (`$SUP state` shows both).

1. Start the next implementer and pre-populate it while the current one works. Every
   task gets a fresh session: `$SUP launch implementer-<n+1>` starts one in its own
   tab, and the supervisor refuses to dispatch a second task to a session that has
   already taken one. Then `$SUP prompt implementer-<n+1> "<reading turn>"` with:

   ```text
   You are about to be given one task in this repository. This turn is preparation
   only: read, then stop. 1. Read these files entirely: [...]. 2. Read these line
   ranges of these large files: [file, range]. 3. This list is a starting frame,
   not a limit. It is short because the rest of the repository is probably
   irrelevant to your task, not because it is off limits — once the task arrives,
   read whatever it turns out you need. 4. This turn is the list and nothing more:
   you do not have the task yet, so anything further is guesswork, and another
   session owns the repository, the build, and git until your task arrives. When
   the list is read, stop and wait.
   ```

   The list is a frame to start from, not everything the implementer will read, so
   size it to what makes the task legible rather than to what it might need. Build it
   from the four slots in "The reading list" above, and keep it under that section's
   budget. Choose against the in-flight implementer's predicted file set: anything it
   will rewrite is read after its commit; large files by line range.
2. The moment the previous task reaches `committed_unverified`, get the next one
   moving — do not wait for its session to fall idle and do not wait to judge its
   commit. Dispatch first, then come back and judge the previous task: `$SUP accept
   <task-id>` runs the checks, and `$SUP accept <task-id> --force --reason "..."`
   bypasses them.
   Neither one gates this dispatch, and nothing forces you to run either; the task's own
   state name is what tells you it is still outstanding. Dispatch with
   `$SUP dispatch <task-id> --to implementer-<n>`. It sends the task verbatim and then
   the implementer's contract, prefixed by "these files changed since your session
   started: [...]" when the tree moved between that session's launch and this dispatch.
   That preamble is unbudgeted and lands on top of the reading turn, so leave room for
   it: the files the in-flight implementer is predicted to change are the files it will
   name. The implementer's contract:

   ```text
   Verify the tree is clean; stop if dirty. Implement only this task. Run the task's
   checks as you work; run the project's quality gate once, immediately before
   committing. Commit without attribution trailers, leave the tree clean, and finish
   with the commit id, changed-file manifest, a one-paragraph semantic delta, and any
   gate failures you judged pre-existing (test name and one-line error).
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
   baseline it carried at dispatch (shown alongside), so the record describes the task
   rather than the session's total. If predictions are far out, size smaller from here
   on.
5. Progress signals come from the supervisor, never self-reports:
   `$SUP state` shows each task's state and each session's measured context;
   `$SUP state --task <task-id>` prints exactly `<task-id> <state>` and nothing else,
   which is the line to watch for `<task-id> committed_unverified`. Every command you
   run ends with `WARNING:` lines on stderr when a measured fact needs you: your
   context near or past 250k, a commit unjudged for five minutes, no state read for
   two minutes while a task is out, no daemon polling. Act on them when they appear;
   nothing is pushed at you.
6. `$SUP poll --after-observation "$OBSERVATION_CURSOR"` returns the commentator's new
   chronological context and every still-unresolved finding. It narrates on its own
   clock; the supervisor alone wakes it with the commit sha and task id, which is a
   trigger to review from git and the implementer log, not a finding or your opinion.
   Never prompt it for a review. A precise finding normally becomes the next fix
   task; you alone decide, and the supervisor remains the authoritative review state.
   A fix task is a task like any other and gets a fresh implementer with its own
   reading list. The reading list is where the author's knowledge is replaced: name the
   commit under the finding and the files it touched, so the fix session sees the shape
   the finding is about before it reads the finding.
7. When an implementer reports failure (gate never green, cannot finish):
   `$SUP abort <task-id> --reason "<its reason>"` records it and checks the
   tree is clean. Read the reason, adjust the task, and retry with a fresh implementer
   (`$SUP task new --retry-of <task-id>`). The supervisor counts aborts across the
   retries; at three it tells you to escalate to the human.
8. A human-flagged trivial edit (trigger word `trivial:`) you do yourself — edit,
   quality gate, commit — only when no implementer is in flight on that file.

Serial wherever it touches the repo: one implementer in flight, one frozen task.

## Stopping

When the user says to stop OR a supervisor command's output warns that your context is past 250k, ask the user once — "Are you sure you want to end the run?" — give them a yes/no choice and take the answer; never infer it. Then, in order:

1. Let the in-flight implementer finish.
2. Wait for the commentator's findings on that commit.
3. Write the continuation prompt to the run directory. Its first line is an
   instruction, not state: "Invoke the `chainsaw-lead` skill and read it in full before
   any other tool call; this file is a state snapshot, not the process." A lead that
   resumed from a continuation without the skill fired implementers as sub-agents and
   in parallel. Then: HEAD, the gate command and
   exact numbers, done/next derived from git not remembered, the exact observation
   cursor, every unresolved finding keyed by its stable number in full, resolved
   findings and their reasons, open questions, traps hit.
4. `$SUP stop`.
