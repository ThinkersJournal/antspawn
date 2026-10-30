// SPDX-License-Identifier: MIT OR Apache-2.0
//! Measure a launcher: `cargo build --bins && cargo run --example measure -- <naive|account|windows-sandbox> [N]`.
//!
//! Reports, for the named launcher (default `naive`, the control):
//! * start latency: `provision` and the first `run` of a no-op job, over N sequential rounds;
//! * cleanup: the median `collect` and `destroy` times, reported beside `provision` and `run`;
//! * concurrency: N jobs that each sleep 2 s, started at once; wall time near 2 s means they ran
//!   together, near 2 s * N means the launcher serialised them (or refused: errors are counted).
//!
//! Prints `MEASURE` lines for `docs/MEASUREMENTS.md`; a launcher that is not set up prints
//! `UNVERIFIED` and exits 0. Nothing here certifies a launcher.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use antspawn::launchers::{Account, Naive, WindowsSandbox};
use antspawn::{Exit, JobSpec, LaunchError, Launcher};

const SLEEP_S: u64 = 2;

fn probe() -> PathBuf {
    let mut p = std::env::current_exe().expect("own path");
    p.pop(); // examples/
    p.pop(); // target/<profile>
    p.join(if cfg!(windows) {
        "spawn-probe.exe"
    } else {
        "spawn-probe"
    })
}

fn spec(id: &str, arg: &[&str]) -> JobSpec {
    if id == "windows-sandbox" {
        return sandbox_spec(arg);
    }
    let host = probe();
    let guest = "spawn-probe.exe";
    // The naive launcher ignores mounts and runs the command on the host, so it needs the real
    // path; the others see the mount at `guest`.
    let exe = if id == "naive" {
        host.display().to_string()
    } else {
        guest.to_owned()
    };
    let cmd: Vec<String> = std::iter::once(exe)
        .chain(arg.iter().map(|a| a.to_string()))
        .map(|a| format!("'{a}'"))
        .collect();
    JobSpec::from_toml(&format!(
        "[job]\nname = \"measure\"\ncmd = [{}]\n[limits]\ntime_s = 60\n[[mount]]\nhost = '{}'\nguest = \"{guest}\"\nmode = \"ro\"\n",
        cmd.join(", "),
        host.display()
    ))
    .expect("measure spec")
}

/// Windows Sandbox maps folders, not files, at absolute guest paths: map a temp folder holding a
/// copy of the probe (the target folder holds far more than the job needs).
fn sandbox_spec(arg: &[&str]) -> JobSpec {
    let dir = std::env::temp_dir().join(format!("antspawn-measure-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("probe dir");
    std::fs::copy(probe(), dir.join("spawn-probe.exe")).expect("copy probe");
    // A clean VM lacks the Visual C++ runtime the probe links dynamically.
    let sys =
        std::path::Path::new(&std::env::var("SystemRoot").expect("SystemRoot")).join("System32");
    for n in ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"] {
        let _ = std::fs::copy(sys.join(n), dir.join(n));
    }
    let cmd: Vec<String> = std::iter::once(r"C:\probe\spawn-probe.exe".to_owned())
        .chain(arg.iter().map(|a| a.to_string()))
        .map(|a| format!("'{a}'"))
        .collect();
    JobSpec::from_toml(&format!(
        "[job]\nname = \"measure\"\ncmd = [{}]\n[limits]\ntime_s = 120\n[[mount]]\nhost = '{}'\nguest = 'C:\\probe'\nmode = \"ro\"\n",
        cmd.join(", "),
        dir.display()
    ))
    .expect("measure spec")
}

fn launcher(id: &str) -> Option<Box<dyn Launcher + Send + Sync>> {
    match id {
        "naive" => Some(Box::new(Naive)),
        "account" => Some(Box::new(Account::default())),
        "windows-sandbox" => Some(Box::new(WindowsSandbox)),
        _ => None,
    }
}

struct Steps {
    provision: Duration,
    run: Duration,
    collect: Duration,
    destroy: Duration,
}

fn one(l: &dyn Launcher, s: &JobSpec, results: &std::path::Path) -> Result<Steps, LaunchError> {
    let t = Instant::now();
    let mut env = l.provision(s)?;
    let provision = t.elapsed();
    let t = Instant::now();
    let exit = env.run(s)?;
    let run = t.elapsed();
    // A job that timed out or failed measured the limit or the failure, not a run: refuse it.
    if exit != Exit::Code(0) {
        return Err(LaunchError::Other(format!("job did not succeed: {exit:?}")));
    }
    let t = Instant::now();
    env.collect(s, results)?;
    let collect = t.elapsed();
    let t = Instant::now();
    env.destroy()?;
    Ok(Steps {
        provision,
        run,
        collect,
        destroy: t.elapsed(),
    })
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_else(|| "naive".into());
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5).max(1);
    let Some(l) = launcher(&id) else {
        eprintln!("unknown launcher `{id}`");
        return ExitCode::from(2);
    };
    if !probe().exists() {
        eprintln!(
            "{} missing: run `cargo build --bins` first",
            probe().display()
        );
        return ExitCode::from(2);
    }
    let results = std::env::temp_dir().join("antspawn-measure");
    let _ = std::fs::remove_dir_all(&results);

    let noop = spec(&id, &["sleep", "0"]);
    let mut steps = Vec::new();
    for i in 0..n {
        match one(l.as_ref(), &noop, &results.join(format!("seq{i}"))) {
            Ok(s) => steps.push(s),
            Err(LaunchError::NotProvisioned { launcher, what }) => {
                println!("UNVERIFIED: launcher {launcher} is not set up: {what}");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                println!("MEASURE launcher={id} sequential round {i} failed: {e}");
                return ExitCode::from(1);
            }
        }
    }
    let ms = |f: fn(&Steps) -> Duration| median(steps.iter().map(f).collect()).as_millis();
    println!(
        "MEASURE launcher={id} rounds={n} median_ms provision={} run={} collect={} destroy={}",
        ms(|s| s.provision),
        ms(|s| s.run),
        ms(|s| s.collect),
        ms(|s| s.destroy)
    );

    let sleeper = spec(&id, &["sleep", &SLEEP_S.to_string()]);
    let t = Instant::now();
    let outcomes: Vec<_> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..n)
            .map(|i| {
                let (l, sleeper, results) = (&l, &sleeper, &results);
                sc.spawn(move || {
                    let r = one(l.as_ref(), sleeper, &results.join(format!("par{i}")));
                    if let Err(e) = &r {
                        println!("MEASURE concurrent job {i} failed: {e}");
                    }
                    r.is_ok()
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or(false)).collect()
    });
    let wall = t.elapsed();
    let ok = outcomes.iter().filter(|&&b| b).count();
    println!(
        "MEASURE launcher={id} concurrent jobs={n} ok={ok} sleep_s={SLEEP_S} wall_ms={} (about {} ms means parallel, about {} ms serial)",
        wall.as_millis(),
        SLEEP_S * 1000,
        SLEEP_S * 1000 * n as u64
    );
    let _ = std::fs::remove_dir_all(&results);
    ExitCode::SUCCESS
}
