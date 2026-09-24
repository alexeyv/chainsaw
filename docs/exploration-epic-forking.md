# Exploration: plan an epic once, implement through context forks

Status: exploration note, 2026-09-06. Nothing here is approved for implementation.
Source: `/tmp/chainsaw-epic-forking-P0O1ZD/handoff.md`, Chainsaw's own supervisor
databases from ten past runs, and one live probe of the Claude Code fork mechanism.

## What Chainsaw does today

Every implementer is a fresh Claude Code session in a Herdr pane. Before its task
arrives it gets a reading turn: a lead-curated list of whole files and line ranges,
budgeted at 60–70 kB of file content. The task is dispatched afterwards with a preamble
naming files that changed since the session launched (`files_changed_since_launch` in
`src/coordinator.rs`, driven by `sessions.launched_head`).

So the seed the handoff describes already exists, in a degenerate form: it is rebuilt
per implementer, and it is a reading list rather than an understanding of the epic.
The calibration table records exactly what that costs.

## What the calibration records say

94 calibrated tasks across ten runs (`calibrations` table in each
`chainsaw-supervisor.db`; rows with `context_size_start = 0` are missing baselines and
excluded from the start figures).

| Measure | Mean | Max |
|---|---|---|
| Context at dispatch (session start plus reading turn) | 50k | 103k |
| Context added by the task itself | 99k | 195k |
| Peak context during the task | 149k | 254k |
| Wall time per task | 21 min | 205 min |
| Lines changed per task | 698 | 8292 |

Three things follow.

- **The reading turn is about 27k tokens of real content per implementer** (50k at
  dispatch minus the roughly 23k a session carries before it reads anything). That is
  the amount forking would stop paying per task. It is real but it is not the
  dominant cost: the task itself adds 99k on average.
- **Tasks already run to 150k peak on average and 250k at worst.** The handoff's
  operating constraint of staying under 150k is violated by the median task today.
  Inheriting a 50–70k seed instead of a 50k reading turn does not change that
  arithmetic; only smaller tasks do. This supports the handoff's suspicion that task
  size is the more useful control.
- **Drift per task is large.** 698 changed lines per task is on the order of 7–10k
  tokens of diff. A seed that is three tasks old has 20–30k tokens of drift, which is
  the size of the reading turn it was meant to replace.

## What the fork mechanism actually does

Probed with `claude --resume <seed-id> --fork-session -p ...` on Claude Code 2.1.263,
a seed session that read two files, and two forks from it. Verified, not assumed:

- **A fork is a file copy.** The fork gets a new session id and a new transcript in the
  same `~/.claude/projects/<munged-cwd>/` directory. That transcript contains every
  entry of the seed, rewritten with the new session id and otherwise identical
  (same `uuid` and `parentUuid` chain, same usage numbers, thinking blocks included),
  followed by the fork's own turns. There is no lineage marker: the seed id does not
  appear anywhere in the fork's file.
- **The seed is untouched.** Two forks from the same seed each saw only the seed's
  history, not each other's. The seed transcript gained no entries.
- **Inherited context is charged at cache-read price while the cache is warm.** The
  seed's prefix was 29k tokens. Each fork's first turn reported 29,024
  `cache_read_input_tokens` and about 400 `cache_creation_input_tokens`. Seed cost
  $0.129 (sonnet, including the file reads); each fork's first turn cost $0.008. A
  fork that arrives after the cache expires pays cache-creation for the whole prefix
  once, and later forks read it again. This is the whole economic case: forks are
  cheap only when they are launched close together in time, or often enough to keep
  the prefix warm.
- **Chainsaw's transcript parser needs no change to observe a fork.** `logs.rs`
  reads context from the last usage entry, so a fork reports the seed's context at
  launch, which is correct. Prompt landing, commit detection and peak measurement work
  on the copied file as they do on a fresh one.
- **Herdr can launch a fork today.** `herdr agent start ... --kind claude -- --resume
  <seed-id> --fork-session <implementer flags>` is the same argv shape
  `session_runtime.rs` already builds. No supervisor code path is missing for the
  launch itself.

What a fork does not inherit: nothing in the transcript describes the repository state
the seed saw beyond what the seed printed into its own turns. The baseline is whatever
HEAD was when the seed read, and Chainsaw would have to record it, the same way it
records `launched_head` for a session now.

## Tradeoffs

**What forking buys.** The 27k-token reading turn per implementer, its wall time
(roughly a minute or two per task), and the lead's time spent composing a reading list
per task. More importantly it can buy something the reading list cannot deliver: the
seed can have thought about the epic, so a fork starts with the shared design in its
head rather than four files and a 2000-token brief.

**What it costs.**

- The seed must be refreshed. Drift accumulates at about one reading turn's worth
  every three tasks, and every fork must be told what moved. Two refresh strategies:
  cold re-seed from HEAD (pays the full seed cost again, resets context), or fork the
  seed itself, feed it the delta, and promote the fork to the new seed (cheap, but the
  seed's context grows every generation, so it has a finite life). A refresh trigger
  could be mechanical: when `git diff <seed-head>..HEAD --stat` restricted to the files
  the seed read exceeds a byte budget, or when the seed's context passes a threshold.
- The delta must reach the fork. Today's preamble names changed files and the session
  rereads them. That is already the cheap form and it composes with a fork unchanged,
  provided the diff base is the seed's head rather than the fork's launch head. Inlining
  diffs would be more expensive than naming files.
- A seed that read broadly carries files the fork does not need. The reading list
  today is task-specific; a seed is epic-specific. For a task touching two files, a
  50–70k seed is a worse starting point than a 20k reading list, and it eats into the
  budget that today's median task already exceeds.
- Cache economics decide whether it is cheap. Sequential chainsaw dispatch, with
  20-minute tasks, leaves a default 5-minute cache cold between forks unless the seed
  is kept warm or the extended cache TTL is in use. Measure this on the real host
  before assuming the probe's numbers hold at run scale.
- Concurrency is a separate question. Forks make concurrent implementers cheap to
  start, but Chainsaw is serial by design (one in-flight task, one repository).
  Concurrent forks need worktrees, integration, and a review protocol that does not
  exist. Keep that out of the first experiment.

## When it is likely to pay

Forking wins when the epic's tasks share most of their reading list, tasks are small
enough that the inherited seed leaves room, and forks are launched close together.
It loses when tasks touch disjoint parts of the tree, when tasks are already large,
or when drift between forks is heavy. Both conditions are measurable from data the
supervisor already collects.

## Refined shape: the task list lives in the seed

Clarified in discussion after the first draft. The seed is not only a repository
reading; it holds the spec and the complete task decomposition, each task boundary
stated in about a hundred tokens. Every fork therefore already knows every task, not
just its own. The dispatch prompt collapses to "you are task N; tasks 1..N-1 have
landed as these commits touching these files". The fork plans and codes from there.

This changes what the design buys, beyond the reading turn:

- **The per-task brief disappears.** Today the lead writes a 2000-token brief and a
  measured reading list per task, and runs the task's checks at the base commit. That
  composition is the lead's main work and the usual reason for a gap between commits.
  With the boundaries already in the seed, the lead sequences and dispatches.
- **Scope discipline is by construction.** A fork knows what its neighbours own, so
  "do not touch X, task 5 owns it" needs no stating. Shared interfaces between tasks
  are decided once, in the seed, where all boundaries are visible together.
- **The delta compresses to task ids and file names.** A fork that already understood
  what task 3 was going to do needs only "task 3 landed in commit abc, files p, q" to
  update its picture, and rereads p and q if its own task overlaps them. No diff needs
  to be inlined. The existing changed-files preamble is already this shape, once its
  base is the seed's head.
- **Planning moves into the fork.** The fork's own plan is made against inherited
  understanding rather than a cold reading, which is the handoff's central claim. The
  seed does the decomposition because it is the session that read the repository; the
  lead no longer needs to.

The constraint that remains is peak context. A fork starts at the seed's size and must
finish inside the budget, so a 60k seed leaves about 90k for the task. Today's mean
task adds 99k, so tasks under this design have to be roughly half today's size. That is
consistent with the intent of smaller sessions, and it is the sizing rule the seed
should be told when it decomposes.

**The implementer's report shrinks to the commit message.** Today's contract asks the
implementer to finish with the commit id, a file manifest, a semantic delta paragraph,
and pre-existing gate failures, all as chat output the lead then reads. Under this
design the implementer writes about 300 tokens of what it did into the commit body,
including any gate failures it judged pre-existing, and says nothing else. Three
things follow:

- The supervisor already detects the commit from git output in the transcript, not
  from the implementer's prose, so a silent implementer costs it nothing.
- The delta for fork N is then literally `git log <seed-head>..HEAD` with bodies: task
  ids, files, and 300 tokens of intent per landed task, written by the session that
  knows best. Ten landed tasks is about 3k tokens of preamble. The lead composes
  nothing.
- The lead stops reading implementer reports at all. It watches task state and
  commentator findings, which is what the review protocol already assumes.

The commit body needs a fixed shape so the next fork and the commentator can rely on
it: task id on the first line, then what changed and why, then pre-existing failures
if any. That shape is a line in the contract, not a supervisor feature.

The seed refresh question becomes narrower: the task list survives a refresh, only the
repository state is stale. A refresh can be a fork of the seed that is told which
tasks landed and rereads the touched files, promoted to the new seed, until its
context grows past the point where a cold seed is cheaper.

## Proposed experiment

A single epic run twice on the same repository and model, comparing today's loop with a
fork-seeded loop. Keep everything else fixed: same stories, same implementer flags,
same commentator, same quality gate, same human steering policy.

**Arm A, cold executors.** The current loop unchanged.

**Arm B, forked executors.** Before the first task, the lead launches one seed session
with the epic-wide reading list plus the spec, tells it to read and think through the
epic, and stops it. Each implementer is then launched with `--resume <seed> --fork-session`
and dispatched with the changed-files preamble computed against the seed's head. The
seed is refreshed on a fixed rule for the experiment: cold re-seed after every third
accepted task.

**Measure per arm, from the supervisor database and git.**

- Tasks accepted, aborted, retried; commits reverted.
- Context at dispatch, peak context, and context added per task (already in
  `calibrations`).
- Wall time per task and total, split into implementer-busy and waiting on human.
- Total tokens by class: fresh input, cache read, cache creation, output. This needs
  the four usage fields summed per session, which `logs.rs` already parses.
- Repeated reading: count of `Read` tool calls per session on files the seed had
  already read.
- Seed overhead in Arm B: seed sessions launched, their tokens and wall time.

**What would falsify it.** Arm B is not worth building if it does not reduce total
tokens or wall time after seed overhead, or if per-task peak context is not lower, or
if abort and retry counts rise. A smaller experiment first: run Arm B's mechanics on
three tasks of a real run to confirm the Herdr launch, the parser, and the preamble
base work before committing to the paired comparison.

**Supervisor changes needed for Arm B, all small.**

- `launch` takes an optional `--fork-of <session>` and appends the resume flags.
- The session row records the seed's head as its diff base when forked.
- `calibrate` or `state` reports token classes, not just context size.

Nothing else. Task sizing, the reading-list budget, and the review protocol stay as
they are so that the comparison isolates the seed.
