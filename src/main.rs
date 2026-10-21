// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn`: the AntSpawn command line. This version can check a spec and print the capabilities
//! it needs; `run` arrives with the first launcher.

use std::path::PathBuf;
use std::process::ExitCode;

use antspawn::JobSpec;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "spawn",
    version,
    about = "Run a job in a clean sandboxed environment"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and validate a job spec, then list the capabilities it needs.
    Check { spec: PathBuf },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check { spec } => {
            let text = match std::fs::read_to_string(&spec) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("spawn: cannot read {}: {e}", spec.display());
                    return ExitCode::from(2);
                }
            };
            match JobSpec::from_toml(&text) {
                Ok(s) => {
                    println!("job `{}` is valid; needs:", s.job.name);
                    for c in s.needs().capabilities {
                        println!("  {c}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("spawn: {e}");
                    ExitCode::from(2)
                }
            }
        }
    }
}
