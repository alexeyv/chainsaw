## CHAINSAW

Agentic software development process, minimizing downtime between coding sessions by pushing planning and review out of the main loop.

## Build and run

Build the coordinator with Cargo:

```sh
cargo build
target/debug/chainsaw --help
```

Run its tests, formatter check, and linter with:

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

## Contract tests

The supervisor has a black-box integration suite that exercises its CLI in isolated
temporary Git repositories. The tests provide an isolated home directory, durable
state directory, session logs, and a stateful fake Herdr executable. They do not import
the supervisor module or depend on its SQLite schema.

Run the suite from the repository root:

```sh
python3 -m unittest discover -s tests -v
```

By default the suite builds the Rust coordinator when needed and runs that binary. To
exercise another implementation of the same CLI contract, set
`CHAINSAW_SUPERVISOR_COMMAND`:

```sh
CHAINSAW_SUPERVISOR_COMMAND='/path/to/another/coordinator' \
  python3 -m unittest discover -s tests -v
```

The command under test must accept the supervisor's existing parent `--run-dir`
argument and subcommands. Git and Python 3 are required to run the fixtures; a real
Herdr installation is not.
