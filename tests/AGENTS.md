# Testing

The coordinator is tested in two layers: unit tests next to the code, and
integration tests through the CLI. Use real objects. Failures must be obvious
from the assertion.

## Unit tests

Prove what each function we bother to test is supposed to do, then how it
deviates. Unit-test what is straightforward at this layer; cover the rest
through integration. Repetition between cases is fine. Do not unit test
trivial getters, functions, or constructors.

### Subject and names

- One subject module per tested function, in the same order as the functions
  in the source file.
- The first case is always `should_work`: the mainline happy path, written to
  demonstrate intended behavior.
- Further cases are deviations: `should_…_when_…`, `should_fail_…_when_…`.
- Names start with `should`, stay on one line, and may be long.

### What to construct

- Use actual objects.
- For the session runtime, extend the zero-cost dummy, or add similarly simple
  dummy files, when a scenario needs more behavior.

### Fixtures

- Build objects with small, domain-named functions that read as a micro DSL
  when composed. Ordinary functions the language already has.
- Put those builders, custom assertions, and similar helpers in one test
  helpers file.
- Keep each fixture as simple as the case needs.
- Unit tests use those helpers. If the object you need is not there yet, add
  another fixture with a discoverable name.

### How much

- Near-100% line coverage where it is not awkward.
- Non-trivial cyclomatic complexity also gets branch coverage and loop
  coverage where possible.

## Integration tests

Prove what the user — an LLM at this CLI — sees and can do.

- Drive the coordinator through its CLI.
- Assert through the CLI. Read SQLite for outcomes the CLI does not show.
- Start from an empty database. Reach any later state by stringing CLI
  commands together.
- Cover domain boundaries, presentation corner cases, and end-to-end results.
- Include at least one full lifecycle: a task is created and taken through to
  accepted, with an assertion after every CLI call.
- Write them as ordinary Rust tests, organized so a human can hand-write them.

## Assertions (both layers)

- Simple values: ordinary equality.
- Non-trivial objects, collections, or row sets: format both sides as
  human-readable strings and compare the strings.
- Where a function has an inverse, test the cycle: start from a state, apply
  both directions, end equivalent. Do this at the layer that owns the property.
