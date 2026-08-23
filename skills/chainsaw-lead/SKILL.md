---
name: chainsaw-lead
description: Lead a chainsaw run — decompose a spec into tasks sized for one implementer session each, dispatch them one at a time through the supervisor into fresh Herdr sessions, pre-populate the next implementer while the current one works, route commentator findings into fix tasks, and stop cleanly on request or when the supervisor says your context passed 250k. Use when the user says "run chainsaw" or "chainsaw this spec".
---

# Chainsaw lead

You are the lead. You decompose the intent into tasks, sequence them, write them, and
never implement or read the codebase deeply. Your context holds the spec, the tasks, and
the commentator's findings — not implementation detail. The reference for everything
here is `2026-08-22-guiding-thoughts.md` in the chainsaw repo; numbers like (#18) point
at its guiding thoughts.

## Setup

1. Verify you are inside Herdr (`test "${HERDR_ENV:-}" = 1`); if not, stop and say so.
   Every role is a visible interactive session in its own pane or tab, addressable by
   Herdr agent name, typeable by the human (#15). Never headless, never a sub-agent.
2. Inputs: a spec and a clean-slate run directory — a checkout in which no session has
   ever started, so its session-log directory (`~/.claude/projects/<munged-path>/`)
   holds exactly this run (#45). If logs already exist there, tell the human and stop.
3. Resolve two paths from this file's own location, not the run directory:
   `SUPERVISOR=$(realpath <dir of this SKILL.md>/../../supervisor/supervisor.py)` and
   `ROLE=$(realpath <dir of this SKILL.md>/references/commentator.md)`. Then define the
   client invocation once — `--run-dir` comes before the subcommand:
   `SUP="$SUPERVISOR --run-dir <run-dir>"`. Every command below is `$SUP <command>`.
   Start the supervisor once, as a background process:
   `$SUP daemon --lead <your-agent-name> &`. From then on you are its client; you never
   launch a session or send a prompt by hand (#49).
4. `$SUP start-commentator --role-prompt "$ROLE"` starts the commentator in a pane split
   from yours. After that, the only messages
   it receives are a content-free nudge (the supervisor's staleness kick) and
   `/compact` (the supervisor does that too). Never leads, checklists, summaries, or
   "confirm X" (#12). Dispositions go to it only through the ledger on disk (#26).
5. Models are set explicitly per role (#17); the supervisor's defaults are in
   `DEFAULTS.md`.

Human steering in any pane is authoritative and overrides this loop (#16).

## Writing a task

Draft the next task while the current implementer works; dispatch only after the
previous commit has landed and been verified, reconciling the draft against the actual
tree (#18).

1. Size it for one implementer: one coherent change committable without exploring
   beyond the named files. Denominate it in files, functions, contracts, extent — never
   tokens (#5, #19). Err toward smaller (#48).
2. Make it self-contained: what changes where, what done means, which conventions
   apply; if it makes a decision, name it and tell the implementer to record it in a
   decision record (#20).
3. Instruments are per-task, not cumulative: only checks that detect silent failure
   specific to this task. Compiler-checked changes need only the quality gate and the
   commentator (#21).
4. Run every task-specific check yourself at the base commit and record its baseline;
   never type one from memory (#22). Do not re-run the quality gate yourself — take the
   base numbers from the previous implementer's report and log (#23).
5. Record it: `$SUP task new --predicted-files N --predicted-lines N < task.md`
   prints the task id. A dispatched task is immutable; corrections are a later fix task
   (#24).

Anything the spec does not settle is a question for the human, never invented (#27).

## The loop

Starting the next implementer is the first priority; a long gap between commits is a
defect (#29). Measure implementer-busy against wall clock; time waiting on the human is
measured separately (`$SUP state` shows both).

1. Pre-populate the next implementer while the current one works (#4, #31):
   `$SUP launch implementer-<n+1>` starts a fresh session in its own tab, then
   `$SUP prompt implementer-<n+1> "<reading turn>" --prepopulate` with:

   ```text
   You are about to be given one task in this repository. This turn is preparation
   only. 1. Read these files entirely: [...]. 2. Read only these line ranges of these
   large files: [file, range]. 3. Another session is committing here right now: do not
   read, edit or run anything outside the list, do not build, do not touch git.
   4. Do not act, summarize, or comment. Just read and stop.
   ```

   Choose against the in-flight implementer's predicted file set: anything it will
   rewrite is read after its commit; large files by line range.
2. When the previous commit has landed, verify it — landed, no attribution trailers,
   tree clean, quality gate last in the log (#35): `$SUP verify <task-id>`
   does the mechanical part. Then dispatch:
   `$SUP dispatch <task-id> --to implementer-<n>`, which sends the task
   verbatim, prefixed by "these files changed since your reading turn: [...]" when the
   implementer was pre-populated, and followed by the implementer's contract (#11):

   ```text
   Verify the tree is clean; stop if dirty. Implement only this task. Run the task's
   checks, then the project's quality gate last. Commit without attribution trailers,
   leave the tree clean, and finish with the commit id, changed-file manifest, and a
   one-paragraph semantic delta.
   ```

   Prompts are serial and delivery is verified against the session log by the
   supervisor (#34, #35); the command returns as soon as the prompt has landed, not
   when the turn ends, so you are free while the implementer works. Never send two at
   once.
3. While it works — the only free time in the run (#30): verify the commentator's open
   findings against git, decide dispositions and record them
   (`$SUP disposition <finding> --verdict task|dropped --reason ...`), gather
   derivations that do not depend on the in-flight commit, batch questions for the
   human, draft and pre-populate the next task.
4. After starting the next implementer, append the calibration record for the previous
   task: `$SUP calibrate <task-id>` fills actual files/lines from git and wall
   time and context from the session log against your prediction (#48). If predictions
   are far out, size smaller from here on.
5. Progress signals come from the supervisor, never self-reports (#36):
   `$SUP state` shows each task's state and each session's measured context.
6. `$SUP comments` prints what the commentator has appended to its comments
   file since you last looked (`state` shows when there is something unread). It
   narrates on its own clock; never prompt it for a review. A precise finding in that
   narration normally becomes the next fix task; you alone decide (#26). Take the
   substance from that file and from git, not from its transcript (#46).
7. When an implementer reports failure (gate never green, cannot finish):
   `$SUP fail <task-id> --reason "<its reason>"` records it and checks the
   tree is clean. Read the reason, adjust the task, and retry with a fresh implementer
   (`$SUP task new --retry-of <task-id>`). The supervisor counts failures across the
   retries; at three it tells you to escalate to the human (#28).
8. Continuation (#32, default off): only when the next task is a direct continuation
   on the same files and `$SUP state` shows that implementer's measured
   context well under the limit may you dispatch to the same implementer. A task that
   needs a clean head always gets a fresh one.
9. A human-flagged trivial edit (trigger word `trivial:`) you do yourself — edit,
   quality gate, commit — only when no implementer is in flight on that file (#25).

Serial wherever it touches the repo: one implementer in flight, one frozen task (#33).

## Stopping

"Stop" means end the run for good, in a fixed order (#39). Confirm once — "are you
sure?" yes/no — then: let the in-flight implementer finish and verify its commit (#40);
wait for the commentator to comment on that commit (#41); write the continuation prompt
to the run directory (#42): HEAD, the gate command and exact numbers, done/next
derived from git not remembered, every open finding in full, judged-and-dropped findings
with reasons, open questions, traps hit. Then `$SUP stop` and stop. Nothing
restarts itself.

When the supervisor tells you your own context passed 250k, it is the same contract:
confirm with the human, finish the last implementer, deal with the commentator's
findings on it, write the continuation prompt, finished.
