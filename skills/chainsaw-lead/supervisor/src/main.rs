use clap::Parser;

use chainsaw::cli::Cli;
use chainsaw::coordinator;
use chainsaw::session_runtime;
use chainsaw::store::Store;

fn main() {
  let cli = Cli::parse();
  let result = session_runtime::from_environment().and_then(|runtime| {
    Store::open(&cli.run_dir)
      .and_then(|store| coordinator::execute(&store, runtime.as_ref(), cli.command))
  });
  if let Err(error) = result {
    if !error.to_string().is_empty() {
      eprintln!("{error}");
    }
    std::process::exit(1);
  }
}
