use clap::Parser;

use chainsaw::cli::Cli;
use chainsaw::coordinator;
use chainsaw::store::Store;

fn main() {
    let cli = Cli::parse();
    let result =
        Store::open(&cli.run_dir).and_then(|store| coordinator::execute(&store, cli.command));
    if let Err(error) = result {
        if !error.to_string().is_empty() {
            eprintln!("{error}");
        }
        std::process::exit(1);
    }
}
