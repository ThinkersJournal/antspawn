// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn`: the AntSpawn command line.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use antspawn::launchers::{self, MATRIX};
use antspawn::{Exit, JobSpec};
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
    /// List every launcher, those that satisfy the spec first, naming what each lacks.
    Providers { spec: PathBuf },
    /// Print the recorded capability matrix.
    Matrix,
    /// Run a job: provision, run, collect outputs, destroy.
    Run {
        spec: PathBuf,
        /// Launcher id; default is the first launcher certified for every need.
        #[arg(long)]
        launcher: Option<String>,
        /// Where declared outputs are copied on the host.
        #[arg(long, default_value = "results")]
        results: PathBuf,
    },
}

fn load(path: &Path) -> Result<JobSpec, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    JobSpec::from_toml(&text).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("spawn: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(command: Command) -> Result<ExitCode, String> {
    match command {
        Command::Check { spec } => {
            let s = load(&spec)?;
            println!("job `{}` is valid; needs:", s.job.name);
            for c in s.needs().capabilities {
                println!("  {c}");
            }
        }
        Command::Providers { spec } => {
            let needs = load(&spec)?.needs();
            for p in launchers::registry().providers(&needs) {
                if p.satisfies() {
                    println!("{}\tsatisfies all needs", p.id);
                } else {
                    let unmet: Vec<_> = p.unmet.iter().map(|c| c.id()).collect();
                    println!("{}\tlacks: {}", p.id, unmet.join(", "));
                }
            }
        }
        Command::Matrix => print!("{MATRIX}"),
        Command::Run {
            spec,
            launcher,
            results,
        } => {
            let s = load(&spec)?;
            let needs = s.needs();
            let registry = launchers::registry();
            let id = match launcher {
                Some(id) => id,
                None => registry
                    .providers(&needs)
                    .into_iter()
                    .find(|p| p.satisfies())
                    .map(|p| p.id)
                    .ok_or("no launcher is certified for every need; see `spawn providers`")?,
            };
            let l = registry
                .open_named(&id, &needs)
                .map_err(|e| e.to_string())?;
            let mut env = l.provision(&s).map_err(|e| e.to_string())?;
            let ran = env.run(&s);
            // Collect and destroy even when the run failed, so nothing is left behind.
            let collected = env.collect(&s, &results);
            let destroyed = env.destroy();
            let exit = ran.map_err(|e| e.to_string())?;
            collected.map_err(|e| e.to_string())?;
            destroyed.map_err(|e| e.to_string())?;
            return Ok(match exit {
                Exit::Code(0) => ExitCode::SUCCESS,
                Exit::Code(c) => ExitCode::from(u8::try_from(c).unwrap_or(1)),
                Exit::TimedOut => {
                    eprintln!("spawn: job timed out");
                    ExitCode::from(124)
                }
                Exit::Killed => ExitCode::from(137),
            });
        }
    }
    Ok(ExitCode::SUCCESS)
}
