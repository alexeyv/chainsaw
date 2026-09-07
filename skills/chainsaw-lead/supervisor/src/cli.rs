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
    /// The lead's agent name.
    #[arg(long)]
    lead: String,
    /// The lead's own Claude Code session id, which names its transcript.
    #[arg(long)]
    session_id: String,
    #[arg(long, default_value_t = 5_000, hide = true)]
    poll_interval_ms: u64,
  },
  /// Start the run's commentator session.
  StartCommentator {
    #[arg(long)]
    role_prompt: PathBuf,
  },
  /// Start an implementer session, or a seed session, or an implementer forked from a seed.
  Launch {
    name: String,
    /// Start a seed: a session that reads the repository and holds the epic's task
    /// map, from which implementers are forked. It never takes a task itself.
    #[arg(long, conflicts_with = "fork_of")]
    seed: bool,
    /// Fork this seed session instead of starting cold; the implementer inherits
    /// the seed's transcript and is dispatched the Git history since the seed's baseline.
    #[arg(long = "fork-of")]
    fork_of: Option<String>,
  },
  /// Hand an idle forked implementer the commits it has not seen, reading only,
  /// so it is caught up before its first task.
  Warm { name: String },
  /// Deliver a prompt to a session.
  Prompt {
    name: String,
    text: String,
    #[arg(long)]
    wait: bool,
    #[arg(long, default_value_t = 300)]
    timeout: u64,
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
  /// Advance a drafted task to dispatched on an implementer session.
  Dispatch {
    task: i64,
    #[arg(long)]
    to: String,
    /// Why this dispatch was made; recorded against the transition.
    #[arg(long)]
    reason: Option<String>,
  },
  /// Accept a task, running the mechanical gate unless it is forced.
  Accept {
    task: i64,
    /// Accept without running the gate. Requires --reason.
    #[arg(long)]
    force: bool,
    /// Why the gate was bypassed. Only meaningful with --force.
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
  /// Print JSON containing new observations, unresolved findings, and task moves.
  Poll {
    /// Return observations after this cursor.
    #[arg(long = "after-observation", default_value_t = 0)]
    after_observation: i64,
    /// Limit findings to this task and observations to this task or the run.
    #[arg(long)]
    task: Option<i64>,
    /// Block until there is something to return: a new observation, a finding
    /// no earlier poll printed, or any task changing state. On timeout, print
    /// what there is and exit 0.
    #[arg(long)]
    wait: bool,
    /// Seconds to wait before giving up. Only meaningful with --wait.
    #[arg(long, default_value_t = 120)]
    timeout: u64,
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
  /// Read or write a run-state flag in the supervisor database.
  Config {
    key: String,
    #[arg(allow_hyphen_values = true)]
    value: Option<String>,
  },
  /// Print current run state.
  State {
    /// Print only this task's id and state name, one line, nothing else.
    #[arg(long)]
    task: Option<i64>,
  },
  /// Print the directory holding this run's session transcripts.
  LogsDir,
  /// Print a line whenever session transcripts grow, paced to one check per interval.
  WatchTranscripts {
    #[arg(long, default_value_t = 120_000)]
    interval_ms: u64,
  },
  /// Print measured context use.
  Context { name: Option<String> },
  /// Open or close a human-wait interval.
  HumanWait { action: HumanWaitAction },
  /// Ask the daemon to stop.
  Stop,
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
    /// Why an active predecessor is being aborted and superseded.
    #[arg(long)]
    reason: Option<String>,
  },
  /// Create drafted tasks from a JSON task map on standard input: an array of
  /// {"text", "files" | "predicted_files", "predicted_lines"} objects, in order.
  Import,
  /// Remedy a coordinator failure to observe an implementer commit.
  RecordCommit {
    task: i64,
    sha: String,
    /// Record the transition manually. Requires --reason.
    #[arg(long)]
    force: bool,
    /// Why the coordinator-driven transition is being remedied.
    #[arg(long)]
    reason: Option<String>,
  },
  /// Remedy a coordinator failure to observe commentary delivery.
  RecordCommentary {
    task: i64,
    /// Record the transition manually. Requires --reason.
    #[arg(long)]
    force: bool,
    /// Why the coordinator-driven transition is being remedied.
    #[arg(long)]
    reason: Option<String>,
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
