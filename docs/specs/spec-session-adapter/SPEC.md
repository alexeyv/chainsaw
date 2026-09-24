---
id: SPEC-session-adapter
companions: [platform-matrix.md, architecture-diagrams.md]
sources: []
---

> **Canonical contract.** This SPEC and the files in `companions:` are the complete, preservation-validated contract for what to build, test, and validate.

# Per-role session adapter: running chainsaw on Codex as well as Claude Code

## Why

An opportunity to capture. Chainsaw orchestrates coding agents, but every session it starts is Claude Code, and that assumption is not confined to a flag — it is spread across the launch arguments, the transcript location, and the transcript parser. The operator wants to compose a run from the best tool per role: a cheap orchestrator, a strong reviewer, and implementers from whichever platform is currently good and affordable. Herdr already starts twenty agent kinds in panes, and Codex is installed and capable, so the only thing standing between the operator and a mixed run is chainsaw's own coupling. Fixing it now, while the supervisor is 4,500 lines and one person holds it in their head, is far cheaper than after a second platform's assumptions have also been absorbed.

## Capabilities

- **CAP-1**
  - **intent:** Each role in a run can run on a different platform and model.
  - **success:** A single run starts a Claude commentator and a Codex implementer from launch flags alone, with no configuration file involved.
- **CAP-2**
  - **intent:** The supervisor observes any supported session correctly, whichever platform wrote its transcript.
  - **success:** Context size, new commits, prompt landing, and sha mentions all resolve correctly against a real Codex rollout and a real Claude transcript.
- **CAP-3**
  - **intent:** A session's platform is durable, so later observation does not depend on who launched it.
  - **success:** A daemon restarted mid-run still parses a Codex implementer's transcript without being told the platform again.
- **CAP-4**
  - **intent:** Supervisor durable state lives in a location chainsaw owns, independent of any platform's transcript directory.
  - **success:** A run whose sessions are all Codex still has a database, and that database is not under `~/.claude`.
- **CAP-5**
  - **intent:** Existing Claude-only runs keep working without the operator learning anything new.
  - **success:** Every command in the current `SKILL.md` works unchanged with no new flags, and a database written before the change opens and reads.
- **CAP-6**
  - **intent:** A heterogeneous run completes end to end.
  - **success:** Claude lead and commentator with Codex implementers takes one task from created to accepted, with commentary delivered against the Codex commit.

## Constraints

- Platform is recorded on the session row. The daemon parses transcripts on every poll, long after launch, so a launch-time flag alone cannot tell it how to read a file.
- `SessionRuntime` keeps its current scope. It is the substrate axis (herdr, and the test dummy) and is already correct; platform behaviour must not be folded into it, or every substrate multiplies by every platform.
- Platform and model are passed as coordinator flags by the lead, never read from a configuration file. The lead already composes these calls; a config file would be a second place to keep in sync with the prompt.
- Defaults preserve today's behaviour: platform defaults to `claude`, model to `opus`, and the schema migration defaults existing session rows to `claude`.
- No single per-run transcript directory may be assumed. Codex partitions by date across all projects, so `logs_dir` must become per-session adapter-resolved paths.
- The supervisor may not push `/compact` unconditionally. Codex auto-compacts and has no such command; compaction pressure is platform-specific behaviour.
- Tool narrowing for implementers cannot rely on a `--disallowedTools` equivalent existing. Codex has none.
- The commentator must be handed resolved transcript paths and the name of the format it is about to read, because in a mixed run it reads a platform it is not itself running on.
- An adapter may report that context size is unavailable, and the supervisor must degrade rather than record zero. Not every platform persists token accounting.
- Every failing test in the gate is a show stopper, per `AGENTS.md`; the shipped supervisor copy under `skills/` stays in sync via `scripts/release.sh`.

## Non-goals

- Grok, Gemini, Cursor, and the other seventeen herdr kinds. Codex only. The trait must not be designed around a third platform that nobody is asking for.
- Subagents, tmux, or orca as substrates. Herdr remains the only real substrate; subagents in particular are ruled out, not deferred — see `platform-matrix.md`.
- Changing what the lead itself runs on. The lead is registered rather than launched, so its platform is whatever the human started, declared to the daemon by flag.
- Retuning the implementer and commentator prompts for Codex's behaviour. This spec delivers the mechanism and one validated run; making Codex implementers as *effective* as Claude ones is empirical work that follows.
- A plugin or dynamic-loading system for adapters. Two compiled-in implementations.

## Success signal

The operator starts a run with a Claude commentator and Codex implementers, walks away, and comes back to an accepted commit reviewed by the commentator — having chosen the platforms with two flags and changed nothing else.

## Assumptions

- The spec folder sits at `docs/specs/` because chainsaw has no `_bmad` installation and therefore no configured output folder.
- `herdr agent get` returns a usable session id for `--kind codex` the way it does for `claude`. This is unverified and is the first thing story 5 must prove, because session registration depends on it.
- Story checkpoints in `stories.yaml` were set without the usual breakdown conversation; they are proposals for the operator to adjust.

## Open Questions

- Where should chainsaw-owned state live — `~/.chainsaw/<munged-run-dir>/`, or inside the run tree (which `AGENTS.md` currently forbids)?
- Should the commentator remain a single Claude session reading two formats, or should its transcript-reading move behind an adapter-provided digest so it never sees raw JSONL?
- Codex `agent_message` payloads can carry `encrypted_content`. Does any commit sha or prompt text chainsaw scrapes for ever land inside one?
