use clap::Parser;

use chainsaw::cli::Cli;
use chainsaw::coordinator;
use chainsaw::persistence::store::Store;
use chainsaw::run::Run;

fn main() {
  let cli = Cli::parse();
  let result = Run::open(&cli.run_dir, &cli.set).and_then(|run| {
    let store = Store::open(run.dir())?;
    coordinator::execute(&run, &store, cli.command)
  });
  if let Err(error) = result {
    if !error.to_string().is_empty() {
      eprintln!("{error}");
    }
    std::process::exit(1);
  }
}
