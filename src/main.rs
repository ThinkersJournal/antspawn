// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn`: the AntSpawn command line.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use antspawn::launchers::{self, MATRIX};
use antspawn::result::Fail;
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
    /// Run the GitHub adapter: turn queued Actions jobs into runners (needs a configuration file).
    Adapter {
        /// The adapter configuration (TOML; schema `configurant.toml`).
        #[arg(long)]
        config: PathBuf,
        /// Validate the configuration and the key path offline, then exit.
        #[arg(long)]
        check: bool,
        /// Poll once, wait for the runner it started, then exit.
        #[arg(long, conflicts_with = "check")]
        once: bool,
        /// The App key is a plain PEM file, not a DPAPI-protected one.
        #[arg(long)]
        pem_key: bool,
    },
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
        Command::Adapter {
            config,
            check,
            once,
            pem_key,
        } => return adapter(&config, check, once, pem_key),
        Command::Run {
            spec,
            launcher,
            results,
        } => return Ok(run_job(&spec, launcher, &results)),
    }
    Ok(ExitCode::SUCCESS)
}

fn adapter(config: &Path, check: bool, once: bool, pem_key: bool) -> Result<ExitCode, String> {
    use antspawn::adapter::command;
    if !check {
        command::run(config, pem_key, once)?;
        return Ok(ExitCode::SUCCESS);
    }
    let report = command::check(&command::load_config(config)?, pem_key);
    for l in &report.lines {
        println!("{l}");
    }
    for p in &report.problems {
        eprintln!("spawn: {p}");
    }
    Ok(if report.problems.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn run_job(spec_path: &Path, launcher: Option<String>, results: &Path) -> ExitCode {
    let mut launcher_id = None;
    let mut ran: Option<Exit> = None;
    let mut streams = antspawn::streams::Streams::default();
    let outcome = attempt(
        spec_path,
        launcher,
        results,
        &mut launcher_id,
        &mut ran,
        &mut streams,
    );
    let v = antspawn::result::verdict(&outcome, ran);
    if let Some(m) = &v.message {
        eprintln!("spawn: {m}");
    }
    write_result(
        results,
        v.status,
        ran,
        launcher_id.as_deref(),
        v.message.as_deref(),
        &streams,
    );
    ExitCode::from(v.code)
}

fn attempt(
    spec_path: &Path,
    launcher: Option<String>,
    results: &Path,
    launcher_id: &mut Option<String>,
    ran: &mut Option<Exit>,
    streams: &mut antspawn::streams::Streams,
) -> Result<(), Fail> {
    let s = load(spec_path).map_err(Fail::Error)?;
    let needs = s.needs();
    let registry = launchers::registry();
    let id = match launcher {
        Some(id) => id,
        None => registry
            .providers(&needs)
            .into_iter()
            .find(|p| p.satisfies())
            .map(|p| p.id)
            .ok_or_else(|| {
                Fail::Refused(
                    "no launcher is certified for every need; see `spawn providers`".to_string(),
                )
            })?,
    };
    *launcher_id = Some(id.clone());
    let l = registry
        .open_named(&id, &needs)
        .map_err(|e| Fail::Refused(e.to_string()))?;
    let mut env = l.provision(&s).map_err(|e| match e {
        antspawn::launcher::LaunchError::NotProvisioned { .. } => Fail::Refused(e.to_string()),
        e => Fail::Error(e.to_string()),
    })?;
    let result = env.run(&s);
    if let Ok(exit) = &result {
        *ran = Some(*exit);
    }
    *streams = env.streams();
    // Collect and destroy even when the run failed, so nothing is left behind.
    let collected = env.collect(&s, results);
    let destroyed = env.destroy();
    result.map_err(|e| Fail::Error(e.to_string()))?;
    collected.map_err(|e| Fail::Error(e.to_string()))?;
    destroyed.map_err(|e| Fail::Error(e.to_string()))?;
    Ok(())
}

/// `<results>/spawn-result.json` plus `stdout.tail` / `stderr.tail`. Best effort: a run that
/// cannot write them still reports through its exit code and stderr.
fn write_result(
    results: &Path,
    status: &str,
    ran: Option<Exit>,
    launcher: Option<&str>,
    message: Option<&str>,
    streams: &antspawn::streams::Streams,
) {
    if let Err(e) = std::fs::create_dir_all(results) {
        eprintln!("spawn: cannot write {}: {e}", results.display());
        return;
    }
    let put = |name: &str, bytes: &Option<Vec<u8>>| -> Option<String> {
        let bytes = bytes.as_ref()?;
        std::fs::write(results.join(name), bytes).ok()?;
        Some(name.to_string())
    };
    let stdout = put("stdout.tail", &streams.stdout);
    let stderr = put("stderr.tail", &streams.stderr);
    let exit_code = match ran {
        Some(Exit::Code(c)) => Some(c),
        _ => None,
    };
    let doc = serde_json::json!({
        "schema": 1,
        "status": status,
        "exit_code": exit_code,
        "launcher": launcher,
        "message": message,
        "streams": { "stdout": stdout, "stderr": stderr, "combined": streams.combined },
    });
    if let Err(e) = std::fs::write(results.join("spawn-result.json"), doc.to_string()) {
        eprintln!("spawn: cannot write spawn-result.json: {e}");
    }
}
