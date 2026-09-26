use clap::Parser;

use chainsaw::cli::Cli;
use chainsaw::coordinator;
use chainsaw::infra::session_runtime;
use chainsaw::infra::settings::Settings;
use chainsaw::infra::store::Store;

fn main() {
  let cli = Cli::parse();
  let runtime = session_runtime::from_environment();
  let result = Settings::load(&cli.run_dir, &cli.set).and_then(|settings| {
    let store = Store::open(&cli.run_dir)?;
    coordinator::execute(&store, runtime.as_ref(), &settings, cli.command)
  });
  if let Err(error) = result {
    if !error.to_string().is_empty() {
      eprintln!("{error}");
    }
    std::process::exit(1);
  }
}
