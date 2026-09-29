# Domain types

**`Run`** (`run.rs`): the run as a whole. Always only one, backed by single row database table. Basically, a persistent store for some state that doesn't have any better home. 

**`RunEvent`** (`run_event.rs`): supervisor's operational journal (launched, kicked, compacted, stopped…).

**`Session`** (`session.rs`): one LLM session.  lead, implementer or commentator. Records the agent it was launched with and keeps it for life. Has zero-to-many Tasks. Borrows the run's `SessionRuntime` and drives itself through it: status, prompt, interrupt, wait.

**`SessionRuntime`** (`session_runtime.rs`): the interface to the terminal multiplexer a run's sessions live in: it starts a session and drives it by name. Implemented in `infra`; the run owns one and hands every `Session` a reference.

**`Task`** (`task.rs`): One unit of work. Typically belongs to an implementer Session, sometimes more than one Task belong to the same implementer Session. May be a retry of another Task. Owns ordered list of TaskEvents. The last TaskEvent in the list determines Task's state. Also owns zero-to-many Findings, Observations, and Calibrations.  
Task is a state machine. Drafted → Dispatched → InFlight → CommittedUnverified → Accepted | Aborted. Transitions may skip forward; Accepted and Aborted are terminal.

**`TaskEvent`** (`task_event.rs`): Record of a Task state transition. Owned by `Task`.

**`Finding`** (`finding.rs`): A concern owned by a Task. Created by a commentator looking at Task's implementation in code; resolved by lead that adds a verdict and - if the verdict is `Task` - creates a fix Task for it.

**`Observation`** (`observation.rs`): Timestamped informational text requiring
no response. Optionally references a `Task`.

**`Prompt`** (`prompt.rs`): One prompt the supervisor sent to a Session: its text, when it was sent, how many times it went out, and when the session's transcript first showed it.

**`HumanWait`** (`human_wait.rs`): One interval the run spent waiting on the human. At most one is open at a time. Knows how long it lasted, or has lasted so far while open.

**`Calibration`** (`calibration.rs`): Measurement record linked to a `Task`. Predicted / actual file and line counts, starting / ending implementer context sizes etc.
