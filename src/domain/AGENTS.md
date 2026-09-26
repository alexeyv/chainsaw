# Domain types

**`Run`** (`run.rs`): the run as a whole. Always only one, backed by single row database table. Basically, a persistent store for some state that doesn't have any better home. 

**`Session`** (`session.rs`): one LLM session.  lead, implementer or commentator. Records the agent it was launched with and keeps it for life. Has zero-to-many Tasks.

**`Task`** (`task.rs`): One unit of work. Typically belongs to an implementer Session, sometimes more than one Task belong to the same implementer Session. May be a retry of another Task. Owns ordered list of TaskEvents. The last TaskEvent in the list determines Task's state. Also owns zero-to-many Findings, Observations, and Calibrations.  
Task is a state machine. Drafted → Dispatched → InFlight → CommittedUnverified → Accepted | Aborted. Transitions may skip forward; Accepted and Aborted are terminal.

**`TaskEvent`** (`task_event.rs`): Record of a Task state transition. Owned by `Task`.

**`Finding`** (`finding.rs`): A concern owned by a Task. Created by a commentator looking at Task's implementation in code; resolved by lead that adds a verdict and - if the verdict is `Task` - creates a fix Task for it.

**`Observation`** (`observation.rs`): Timestamped informational text requiring
no response. Optionally references a `Task`.

**`Calibration`** (`calibration.rs`): Measurement record linked to a `Task`. Predicted / actual file and line counts, starting / ending implementer context sizes etc.
