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
  /// Mark an in-flight task as failed.
  Fail {
    task: i64,
    #[arg(long)]
    reason: String,
  },
  /// Dispatch a drafted task.
  Dispatch {
    task: i64,
    #[arg(long)]
    to: String,
    #[arg(long)]
    reuse: bool,
  },
  /// Verify a task's commit and quality gate.
  Verify { task: i64 },
  /// Accept a committed task despite a verification false positive.
  Accept {
    task: i64,
    #[arg(long)]
    reason: String,
  },
  /// Record predicted and actual task size.
  Calibrate { task: i64 },
  /// Record a commentator finding's disposition.
  Disposition {
    /// Task where the finding originated.
    task_id: i64,
    description: String,
    #[arg(long)]
    verdict: Verdict,
    #[arg(long = "fix-task")]
    fix_task_id: Option<i64>,
    #[arg(long = "reason")]
    verdict_reason: String,
  },
  /// Read or write a run setting.
  Config {
    key: String,
    #[arg(allow_hyphen_values = true)]
    value: Option<String>,
  },
  /// Print current run state.
  State,
  /// Print new commentator entries.
  Comments {
    #[arg(long)]
    all: bool,
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
