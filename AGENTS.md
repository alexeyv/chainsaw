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
- How to write tests: `tests/AGENTS.md` 

## Running and verifying

- `--run-dir` is a parent flag and must precede the subcommand: `target/debug/chainsaw --run-dir DIR <subcommand>`
- Build the supervisor with `cargo build`.
- Compile the probe with `mkdir -p tools/bin && clang++ -o tools/bin/context-probe tools/context-probe.cc` — no Makefile.

## Quality gate

Run the complete gate from the repository root, in this order:

```sh
cargo fmt --check
cargo clippy --quiet --all-targets --all-features --locked -- -D warnings
cargo test --quiet --locked
python3 -m unittest discover -s tests -q
mkdir -p tools/bin
clang++ -o tools/bin/context-probe tools/context-probe.cc
```

How tests should be written is in `tests/AGENTS.md`.

- Every failing automatic test is a show stopper, even if unrelated to work at hand. Fix or escalate, never ignore.

## Conventions that differ from defaults

- Supervisor and commentator durable state lives under `~/.claude/projects/<munged-run-dir>/`, never in the run tree.

## Error handling

- Let errors propagate to the top of the call stack by default.
- In Rust, use `anyhow::Error` and `anyhow::Result` as the default catch-all error type,
  adding context where it helps explain the failure.
- Create a custom error type or enum variant only when a caller actually needs to
  match it and take different action. Tests matching a variant do not justify an error
  taxonomy on their own.
