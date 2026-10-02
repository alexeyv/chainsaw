//! The terminal multiplexers a run's sessions can live in. Which one a run
//! uses is decided where the run is opened. Each is driven through its own
//! CLI, and what driving a CLI that answers in JSON takes is shared here.

use std::ffi::OsString;
use std::process::{Command, Output};

use anyhow::{Context, Result};
use serde_json::Value;

mod herdr;
mod orca;

pub use herdr::HerdrSessionRuntime;
pub use orca::OrcaSessionRuntime;

/// A multiplexer's command-line interface: the program to run, and the name
/// its failures are reported under.
struct Cli {
  name: &'static str,
  program: OsString,
}

impl Cli {
  fn new(name: &'static str, program: impl Into<OsString>) -> Self {
    Self {
      name,
      program: program.into(),
    }
  }

  /// Runs the program with `args` and captures what it printed, whatever its
  /// exit status.
  fn run(&self, args: &[&str]) -> Result<Output> {
    Command::new(&self.program)
      .args(args)
      .output()
      .with_context(|| format!("failed to run {}", self.program.to_string_lossy()))
  }

  /// The string at `pointer` in a reply, or why there is none.
  fn json_string(&self, value: &Value, pointer: &str) -> Result<String> {
    value
      .pointer(pointer)
      .and_then(Value::as_str)
      .map(str::to_owned)
      .with_context(|| format!("{} response lacks {pointer}", self.name))
  }
}

#[cfg(test)]
mod tests;
