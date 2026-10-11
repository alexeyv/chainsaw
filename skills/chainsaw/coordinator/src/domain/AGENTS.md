# Domain types

A map, not a spec: per entity, what it holds, what it belongs to, what it is for. No implementation details.

**`Run`** (`run.rs`): the run as a whole. Always only one. A persistent store for state that has no better home.

**`RunEvent`** (`run_event.rs`): supervisor's operational journal (launched, kicked, compacted, stopped…).

**`Session`** (`session.rs`): one LLM session of the run: lead, implementer or commentator. Has zero-to-many Tasks. Holds the agent kind it was launched with and its `Transcript`. Uses the run's `SessionRuntime` and `Agent`.

**`Agent`** (`agent.rs`): the interface to the coding CLI a session runs: starting a session and reading its transcript. One per `AgentKind`.

**`Transcript`** (`transcript.rs`): the interface to what a session's transcript tells the supervisor: size, context, prompts taken, what the agent said, commits made. Owned by a `Session`.

**`SessionRuntime`** (`session_runtime.rs`): the interface to the terminal multiplexer a run's sessions live in. One per run.

**`Task`** (`task.rs`): One unit of work. Typically belongs to an implementer Session, sometimes more than one Task belong to the same implementer Session. May be a retry of another Task. Owns ordered list of TaskEvents. Also owns zero-to-many Findings, Observations, and Calibrations.  
Task is a state machine: Drafted → Dispatched → InFlight → CommittedUnverified → Accepted | Aborted.

**`TaskEvent`** (`task_event.rs`): Record of a Task state transition. Owned by `Task`.

**`Finding`** (`finding.rs`): A concern a commentator raised about a Task's implementation. Owned by that Task; resolved by the lead with a verdict.

**`Observation`** (`observation.rs`): Timestamped informational text requiring
no response. Optionally references a `Task`.

**`Prompt`** (`prompt.rs`): One prompt the supervisor sent to a Session: its text, when it was sent, how many times it went out, and when the session's transcript first showed it.

**`HumanWait`** (`human_wait.rs`): One interval the run spent waiting on the human. Knows how long it lasted, or has lasted so far while open.

**`Calibration`** (`calibration.rs`): Measurement record linked to a `Task`. Predicted / actual file and line counts, starting / ending implementer context sizes etc.
