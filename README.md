# Chainsaw

<p align="center">
  <img src="docs/schematic.jpeg" alt="Chainsaw system schematic" width="640">
</p>

One day the author was running the usual supervised, highly automated spec-driven loop — actually, furiously context-switching between three of them — and thought: why, most of the time, is nothing writing any code?

Plan, review, code, review, fix, review, a human checkpoint, rinse repeat. See how the actual coding is just a small part of the cycle? Sure, the rest of it saves production from several nasty bugs a day, but this also means that you're either waiting all the time, or context-switching every ten minutes.

Chainsaw is an attempt to cure the context switching. Here is how it works:

**Lead** cuts the work into tasks one session can finish, and keeps handing them out so that some LLM session is always writing code.

**Implementers** write the code. Well, one of them does. The next is already reading the files it'll need. The first implementer commits, the lead hands the next one its own task, and warms up another.

**Commentator** watches the commits and the session logs. Anything wrong or just weird goes to the lead, which can turn it into the next task.

**You** watch the code change and steer the lead — this next, not that, stop here. You stay on this run because new code arrives at a pace that keeps you engaged.

There is some deterministic glue: a CLI, a daemon, and an SQLite database, keeping tabs on the run and correcting LLM sessions when they lose track or run out of usable context.

When the run is over — say, ten stories later — the lead hands you a continuation prompt for the next run, and a summary of what happened. Review, push, PR, deploy, learn lessons: that's you. Chainsaw is done.

## Install

Requires [Herdr](https://herdr.dev) and a Rust toolchain (the supervisor builds itself on first use). Then:

```sh
npx skills add alexeyv/chainsaw
```

Prepare a spec, preferably with a story breakdown. Say **chainsaw this**. Or start it and feed small intents by hand.
