# Testing

Test each behavior at the cheapest layer that can demonstrate it. Use real
objects and make failures obvious from the assertion.

## Rust tests

- Keep tests next to the code that owns the behavior. Use one subject module
  per tested function, in source order.
- Name the mainline case `should_work`. Name deviations
  `should_…_when_…` or `should_fail_…_when_…`.
- Cover meaningful state transitions, boundary conditions, and failure paths.
  Skip trivial getters and constructors. When several layers call the same
  logic, cover its edge cases once.
- Keep fixtures as small as the case needs. Factor out shared builders and
  custom assertions when they make the behavior easier to read.
- Use ordinary equality for simple values. For complex objects or row sets,
  compare human-readable representations. Test inverse operations together
  at the layer that owns the round trip.

## Python smoke test

Keep one end-to-end task lifecycle through the shipped CLI. Start with an
empty run, launch an implementer, and dispatch its task. Wait for the daemon
to record its Git commit and verify acceptance from a later process. A small
Herdr fixture on PATH stands in for the terminal and agent. Specific runtime
protocols and transcript formats belong in their Rust adapter tests.
