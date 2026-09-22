use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "eggreplay",
    version,
    about = "Semantic HTTP recording, replay, and regression testing"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the current tool version.
    Version,
}

fn main() {
    let cli = Cli::parse();
    if matches!(cli.command, Some(Command::Version)) {
        println!("{}", env!("CARGO_PKG_VERSION"));
    }
}
