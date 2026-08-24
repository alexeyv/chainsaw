## CHAINSAW

Agentic software development process, minimizing downtime between coding sessions by pushing planning and review out of the main loop.

## Contract tests

The supervisor has a black-box integration suite that exercises its CLI in isolated
temporary Git repositories. The tests provide an isolated home directory, durable
state directory, session logs, and a stateful fake Herdr executable. They do not import
the supervisor module or depend on its SQLite schema.

Run the suite from the repository root:

```sh
python3 -m unittest discover -s tests -v
```

By default the suite runs `supervisor/supervisor.py`. To exercise another
implementation of the same CLI contract, set `CHAINSAW_SUPERVISOR_COMMAND`:

```sh
CHAINSAW_SUPERVISOR_COMMAND='target/debug/chainsaw' \
  python3 -m unittest discover -s tests -v
```

The command under test must accept the supervisor's existing parent `--run-dir`
argument and subcommands. Git and Python 3 are required to run the fixtures; a real
Herdr installation is not.
