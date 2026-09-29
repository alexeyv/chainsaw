//! The terminal multiplexers a run's sessions can live in. Which one a run
//! uses is decided where the run is opened.

mod herdr;
mod orca;

pub use herdr::HerdrSessionRuntime;
pub use orca::OrcaSessionRuntime;
