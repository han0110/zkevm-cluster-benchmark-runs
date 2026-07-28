//! Binary entry point. Parses CLI args and dispatches to the subcommands.

use clap::Parser;
use tools::parse_benchmark::{self, ParseBenchmarkArgs};

/// The CLI root. Subcommands are added as Command variants.
#[derive(clap::Parser)]
#[command(name = "tools", about = "zkVM cluster benchmark tooling", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Parse a zkVM benchmark run directory into a lean benchmark.json.
    ParseBenchmark(ParseBenchmarkArgs),
}

fn main() {
    match Cli::parse().command {
        Command::ParseBenchmark(args) => run_parse_benchmark(&args),
    }
}

/// Runs the parse-benchmark subcommand and reports the outcome.
fn run_parse_benchmark(args: &ParseBenchmarkArgs) {
    let output = args.output_path();
    let verb = if args.patch { "patched" } else { "wrote" };
    match parse_benchmark::run(&args.input, &output, args.force, args.patch) {
        Ok(count) => eprintln!(
            "{verb} {} ({} runs, {count} blocks)",
            output.display(),
            args.input.len()
        ),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}
