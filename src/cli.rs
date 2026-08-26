use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(about = "Coordinate a Chainsaw development run")]
pub struct Cli {
  /// The run's clean-slate checkout.
  #[arg(long, global = true, default_value = ".")]
  pub run_dir: PathBuf,

  #[command(subcommand)]
  pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
  /// Run the background coordinator.
  Daemon {
    #[arg(long)]
    lead: String,
    #[arg(long, default_value_t = 5_000, hide = true)]
    poll_interval_ms: u64,
  },
  /// Start the run's commentator session.
  StartCommentator {
    #[arg(long)]
    role_prompt: PathBuf,
  },
  /// Start an implementer session.
  Launch {
    name: String,
    #[arg(long)]
    fresh: bool,
    #[arg(long)]
    reason: Option<String>,
  },
  /// Deliver a prompt to a session.
  Prompt {
    name: String,
    text: String,
    #[arg(long)]
    wait: bool,
    #[arg(long, default_value_t = 300)]
    timeout: u64,
    #[arg(long)]
    prepopulate: bool,
  },
  /// Manage tasks.
  Task {
    #[command(subcommand)]
    action: TaskCommand,
  },
  /// Abort a task that will not produce an accepted commit.
  Abort {
    task: i64,
    #[arg(long)]
    reason: String,
  },
  /// Advance a task to its next lifecycle state.
  Advance {
    task: i64,
    /// Target state. Advancing to the task's current state is a no-op.
    state: AdvanceState,
    /// Implementer session to dispatch to (state `dispatched`).
    #[arg(long)]
    to: Option<String>,
    /// Dispatch to a session that has already taken a task.
    #[arg(long)]
    reuse: bool,
    /// Why this transition was made. For `accepted`, giving a reason overrides
    /// the mechanical gate instead of running it.
    #[arg(long)]
    reason: Option<String>,
  },
  /// Record predicted and actual task size.
  Calibrate { task: i64 },
  /// Record informational context that requires no response.
  Observe {
    /// Task the observation concerns; omit for a run-wide observation.
    #[arg(long)]
    task: Option<i64>,
    text: String,
  },
  /// Register a concern that requires a verdict and reason.
  Finding {
    #[arg(long)]
    task: i64,
    description: String,
  },
  /// Print JSON containing new observations and unresolved findings.
  Poll {
    /// Return observations after this cursor.
    #[arg(long = "after-observation", default_value_t = 0)]
    after_observation: i64,
    /// Limit findings to this task and observations to this task or the run.
    #[arg(long)]
    task: Option<i64>,
  },
  /// Resolve a supervisor-mediated finding.
  Resolve {
    finding: i64,
    #[arg(long)]
    verdict: Verdict,
    #[arg(long = "fix-task")]
    fix_task_id: Option<i64>,
    #[arg(long)]
    reason: String,
  },
  /// Print JSON containing all resolved findings.
  Resolutions,
  /// Read or write a run setting.
  Config {
    key: String,
    #[arg(allow_hyphen_values = true)]
    value: Option<String>,
  },
  /// Print current run state.
  State,
  /// Print measured context use.
  Context { name: Option<String> },
  /// Open or close a human-wait interval.
  HumanWait { action: HumanWaitAction },
  /// Ask the daemon to stop.
  Stop,
}

/// Lifecycle states a caller may advance a task to. `in_flight` and `committed`
/// are recorded by the supervisor itself and are not addressable here.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AdvanceState {
  Dispatched,
  Accepted,
}

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
  /// Create a drafted task from standard input.
  New {
    #[arg(long)]
    files: Option<String>,
    #[arg(long)]
    predicted_files: Option<i64>,
    #[arg(long)]
    predicted_lines: i64,
    #[arg(long = "retry-of")]
    retry_of_task_id: Option<i64>,
  },
}

#[derive(Clone, Debug, ValueEnum)]
pub enum Verdict {
  Task,
  Dropped,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum HumanWaitAction {
  Start,
  End,
}
