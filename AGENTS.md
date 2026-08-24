## CHAINSAW

Agentic software development process, minimizing downtime between coding sessions by pushing planning and review out of the main loop.

## Policy

- Conventional commits; subject line at most 72 characters.
- Never push unless the human explicitly asks to push.

## Where things are

- Lead prompt: `skills/chainsaw-lead/SKILL.md`
- Commentator prompt: `skills/chainsaw-lead/references/commentator.md`
- Supervisor binary: `target/debug/chainsaw`, built from `src/`
- Context probe: `tools/bin/context-probe` built from `tools/context-probe.cc` 

## Running and verifying

- `--run-dir` is a parent flag and must precede the subcommand: `target/debug/chainsaw --run-dir DIR <subcommand>`
- Build the supervisor with `cargo build`.
- Compile the probe with `mkdir -p tools/bin && clang++ -o tools/bin/context-probe tools/context-probe.cc` — no Makefile.

## Conventions that differ from defaults

- Supervisor and commentator durable state lives under `~/.claude/projects/<munged-run-dir>/`, never in the run tree.
