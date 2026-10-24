// SPDX-License-Identifier: MIT OR Apache-2.0
//! Regenerate `matrix/launchers.tsv`: `cargo build --bins && cargo run --example matrix [-- --check]`.
//!
//! Runs the negative probes against every launcher that can run unprivileged (`naive`) and writes
//! one `capability` row per launcher and capability, through CapAnt's `generate`. **Nothing is
//! `yes` without a probe that ran, passed and named itself**: a probe that found the isolation
//! broken gives `no` with the probe named, a probe whose positive control failed gives
//! `UNVERIFIED`, and a capability with no probe yet is untested. `--check` writes nothing and exits
//! 1 if the committed file differs from the probes (what CI runs).
//!
//! `account` is probed only on a Windows machine where its setup has been applied (privileged, so CI
//! cannot); `windows-sandbox` is not probed from here at all. Where a launcher cannot be probed, CapAnt
//! carries its committed rows over (never downgraded), but only if all of them are present and every
//! `yes` names the probe that earned it; the measured run is recorded in `docs/MEASUREMENTS.md`.

use std::path::PathBuf;
use std::process::ExitCode;

use antspawn::launchers::Naive;
use antspawn::probes::{NET_ADDR, Probes};
use antspawn::{Capability, Launcher};
use capant::{
    CapabilityId, Config, Context, Evidence, Matrix, Outcome, Prober, Provider, Reason, Surface,
    Why,
};

const FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/matrix/launchers.tsv");

/// A probe that ran: `violated` means the isolation property was broken.
fn verdict(violated: bool, probe: &str, broken: &str) -> Outcome {
    let evidence = Evidence::probe(probe);
    if violated {
        Outcome::Fail {
            reason: Reason::Other(broken.into()),
            evidence,
        }
    } else {
        Outcome::Pass(evidence)
    }
}

/// The network probe has no answer when the host cannot reach the probe address at all.
fn network(violated: Option<bool>) -> Outcome {
    match violated {
        Some(v) => verdict(
            v,
            "connect",
            "job opened a TCP connection with the network supposedly off",
        ),
        None => Outcome::Unverified {
            why: Why::NoControl,
            evidence: Evidence::probe("connect"),
        },
    }
}

fn untested() -> Outcome {
    Outcome::Unverified {
        why: Why::Untested,
        evidence: Evidence::default(),
    }
}

struct Matrixer {
    probes: Probes,
    #[cfg_attr(not(windows), allow(dead_code))]
    exe: PathBuf,
    /// Scratch files for the `naive` probes.
    dir: PathBuf,
    sentinel: PathBuf,
}

impl Matrixer {
    fn new(exe: PathBuf) -> Self {
        let dir = std::env::temp_dir().join(format!("antspawn-matrix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let sentinel = dir.join("outside.txt");
        std::fs::write(&sentinel, b"secret").expect("sentinel");
        let _ = std::fs::remove_dir_all(Naive::work_dir());
        Matrixer {
            probes: Probes::new(exe.clone()),
            exe,
            dir,
            sentinel,
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(Naive::work_dir());
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// `naive`: every probe that exists is run.
    fn naive(&self, cap: Capability) -> Outcome {
        let p = &self.probes;
        let l: &dyn Launcher = &Naive;
        match cap {
            Capability::NoReadOutside => verdict(
                p.violates_read(l, &self.sentinel),
                "read",
                "job read a host file outside its environment",
            ),
            Capability::NoWriteOutside => verdict(
                p.violates_write(l, &self.dir.join("written.txt")),
                "write",
                "job wrote a host file outside its environment",
            ),
            Capability::CleanSlateNextRun => verdict(
                p.violates_clean_slate(l),
                "write+exists",
                "a file written by one job existed for the next",
            ),
            Capability::KillsRunawayProcess => verdict(
                p.violates_kill_runaway(l, &self.dir.join("pid")),
                "fork",
                "a forked process survived the environment",
            ),
            Capability::NetworkOff => network(p.violates_network(l, NET_ADDR)),
            _ => untested(),
        }
    }

    /// `account`, measured: every probe that exists runs through the real restricted account.
    #[cfg(windows)]
    fn account(&self, cap: Capability) -> Outcome {
        use antspawn::probes::account as a;
        let probe = self.exe.as_path();
        match cap {
            Capability::NoReadOutside => verdict(
                a::violates_read(probe),
                "read",
                "job read a host file outside its environment",
            ),
            Capability::NoWriteOutside => verdict(
                a::violates_write(probe),
                "write",
                "job wrote a host file outside its environment",
            ),
            Capability::CleanSlateNextRun => verdict(
                a::violates_clean_slate(probe),
                "write+exists",
                "a file written by one job existed for the next",
            ),
            Capability::KillsRunawayProcess => verdict(
                a::violates_kill_runaway(probe),
                "fork",
                "a forked process survived the environment",
            ),
            Capability::NetworkOff => network(a::violates_network(probe, NET_ADDR)),
            _ => untested(),
        }
    }
}

impl Prober for Matrixer {
    fn can_probe(&self, provider: &Provider) -> bool {
        match provider.id() {
            "naive" => true,
            // `--no-privileged` forces the carry-over path, which is what a machine without the setup (CI) takes.
            #[cfg(windows)]
            "account" => {
                !std::env::args().any(|a| a == "--no-privileged")
                    && antspawn::probes::account::set_up()
            }
            // No probe run drives the sandbox yet (it needs an interactive desktop): never `yes` from here.
            _ => false,
        }
    }

    fn probe(&self, provider: &Provider, cap: &CapabilityId, _ctx: &Context) -> Outcome {
        let Ok(cap) = cap.as_str().parse::<Capability>() else {
            return untested();
        };
        match provider.id() {
            "naive" => self.naive(cap),
            #[cfg(windows)]
            "account" => self.account(cap),
            _ => untested(),
        }
    }
}

fn probe_exe() -> PathBuf {
    let exe = std::env::current_exe().expect("own path");
    // target/<profile>/examples/matrix -> target/<profile>/spawn-probe
    let dir = exe.parent().and_then(|p| p.parent()).expect("target dir");
    dir.join(format!("spawn-probe{}", std::env::consts::EXE_SUFFIX))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn main() -> ExitCode {
    let check = std::env::args().any(|a| a == "--check");
    let exe = probe_exe();
    if !exe.exists() {
        eprintln!(
            "{} not found: run `cargo build --bins` first",
            exe.display()
        );
        return ExitCode::from(2);
    }

    let table: Vec<Provider> = ["naive", "account", "windows-sandbox"]
        .iter()
        .map(|id| Provider::new(*id, vec![Surface::new(*id, "1", Config::new())]).expect("surface"))
        .collect();
    let caps: Vec<CapabilityId> = Capability::ALL.iter().map(|c| c.cap_id()).collect();
    // The first rows to carry an `environment` column come with the first real regeneration (a
    // separate commit), so this context names none and the committed rows stay as they are.
    let ctx = Context::new(now_secs(), Config::new());

    let committed_text = std::fs::read_to_string(FILE)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    let committed = antspawn::matrix::parse(&committed_text).unwrap_or_default();
    let prober = Matrixer::new(exe);
    let generated = capant::generate(&table, &caps, &prober, &committed, &ctx);
    prober.cleanup();
    let text = capant::write(&generated);
    // Whatever was generated must be accepted by the reader that will certify launchers from it.
    let parsed = antspawn::matrix::parse(&text).expect("generated matrix parses");
    assert_eq!(capant::write(&parsed), text, "generated text is canonical");

    if check {
        // CapAnt marks carried-over `yes` rows inherited, which adds an `inherited_from` column the
        // committed file does not have yet. That column arrives with the first real regeneration (a
        // separate commit); until then the check compares without it.
        let comparable = Matrix::from_rows(
            generated
                .rows()
                .iter()
                .cloned()
                .map(|mut r| {
                    r.evidence.inherited_from = None;
                    r
                })
                .collect(),
        );
        let report = capant::check(&committed_text, &comparable, &ctx);
        if report.is_ok() {
            println!("matrix/launchers.tsv is up to date");
            ExitCode::SUCCESS
        } else {
            eprintln!(
                "matrix/launchers.tsv differs from the generated matrix: {report:#?}\n{text}"
            );
            ExitCode::FAILURE
        }
    } else {
        std::fs::write(FILE, &text).expect("write matrix");
        println!("wrote {FILE}");
        ExitCode::SUCCESS
    }
}
