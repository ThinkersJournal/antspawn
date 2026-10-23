// SPDX-License-Identifier: MIT OR Apache-2.0
//! Regenerate `matrix/launchers.tsv`: `cargo build --bins && cargo run --example matrix [-- --check]`.
//!
//! Runs the negative probes against every launcher that can run unprivileged (`naive`) and writes
//! one `capability` row per launcher and capability. **Nothing is `yes` without a probe that ran
//! and passed**: a probe that found the isolation broken gives `no` with the probe named, a launcher
//! that is not set up gives `no` with `UNVERIFIED`, and a capability with no probe yet gives `no`.
//! `--check` writes nothing and exits 1 if the committed file differs (what CI runs).
//!
//! `account` is probed only on a Windows machine where its setup has been applied (privileged, so CI
//! cannot). Anywhere else its committed rows are carried over unchanged, never downgraded, but only if
//! all of them are present and every `yes` names the probe that earned it; the measured run is recorded
//! in `docs/MEASUREMENTS.md`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use antspawn::launchers::Naive;
use antspawn::matrix::{HEADER, Matrix, Row};
use antspawn::probes::{NET_ADDR, Probes};
use antspawn::{Capability, Launcher};

const FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/matrix/launchers.tsv");

fn row(launcher: &str, cap: Capability, yes: bool, reason: &str, probe: &str) -> Row {
    Row {
        subject_kind: "capability".into(),
        subject: cap.id().into(),
        backend: launcher.into(),
        version: "1".into(),
        config: String::new(),
        enabled: yes,
        reason: reason.into(),
        metadata: if probe.is_empty() {
            String::new()
        } else {
            format!("probe={probe}")
        },
        note: String::new(),
    }
}

/// `naive`: every probe that exists is run; `violated` means the isolation property was broken.
fn naive(probes: &Probes) -> Vec<Row> {
    let dir = std::env::temp_dir().join(format!("antspawn-matrix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let sentinel = dir.join("outside.txt");
    std::fs::write(&sentinel, b"secret").expect("sentinel");
    let _ = std::fs::remove_dir_all(Naive::work_dir());

    let l: &dyn Launcher = &Naive;
    let result = |cap: Capability, violated: bool, probe: &str, broken: &str| {
        if violated {
            row("naive", cap, false, broken, probe)
        } else {
            row("naive", cap, true, "", probe)
        }
    };
    let mut rows = Vec::new();
    for &cap in Capability::ALL {
        rows.push(match cap {
            Capability::NoReadOutside => result(
                cap,
                probes.violates_read(l, &sentinel),
                "read",
                "job read a host file outside its environment",
            ),
            Capability::NoWriteOutside => result(
                cap,
                probes.violates_write(l, &dir.join("written.txt")),
                "write",
                "job wrote a host file outside its environment",
            ),
            Capability::CleanSlateNextRun => result(
                cap,
                probes.violates_clean_slate(l),
                "write+exists",
                "a file written by one job existed for the next",
            ),
            Capability::KillsRunawayProcess => result(
                cap,
                probes.violates_kill_runaway(l, &dir.join("pid")),
                "fork",
                "a forked process survived the environment",
            ),
            Capability::NetworkOff => match probes.violates_network(l, NET_ADDR) {
                Some(v) => result(
                    cap,
                    v,
                    "connect",
                    "job opened a TCP connection with the network supposedly off",
                ),
                None => row(
                    "naive",
                    cap,
                    false,
                    "UNVERIFIED: this host cannot reach the probe address, so there is no control",
                    "connect",
                ),
            },
            _ => row("naive", cap, false, "no probe for this capability yet", ""),
        });
    }
    let _ = std::fs::remove_dir_all(Naive::work_dir());
    let _ = std::fs::remove_dir_all(dir);
    rows
}

/// A launcher whose privileged setup we do not drive from here: never `yes`.
fn unverified(launcher: &str) -> Vec<Row> {
    Capability::ALL
        .iter()
        .map(|&cap| row(launcher, cap, false, "UNVERIFIED: not set up", ""))
        .collect()
}

/// `account`, measured: every probe that exists runs through the real restricted account.
#[cfg(windows)]
fn account_measured(probe: &Path) -> Vec<Row> {
    use antspawn::probes::account as a;
    let result = |cap: Capability, violated: bool, probe: &str, broken: &str| {
        if violated {
            row("account", cap, false, broken, probe)
        } else {
            row("account", cap, true, "", probe)
        }
    };
    Capability::ALL
        .iter()
        .map(|&cap| match cap {
            Capability::NoReadOutside => result(
                cap,
                a::violates_read(probe),
                "read",
                "job read a host file outside its environment",
            ),
            Capability::NoWriteOutside => result(
                cap,
                a::violates_write(probe),
                "write",
                "job wrote a host file outside its environment",
            ),
            Capability::CleanSlateNextRun => result(
                cap,
                a::violates_clean_slate(probe),
                "write+exists",
                "a file written by one job existed for the next",
            ),
            Capability::KillsRunawayProcess => result(
                cap,
                a::violates_kill_runaway(probe),
                "fork",
                "a forked process survived the environment",
            ),
            Capability::NetworkOff => match a::violates_network(probe, NET_ADDR) {
                Some(v) => result(
                    cap,
                    v,
                    "connect",
                    "job opened a TCP connection with the network supposedly off",
                ),
                None => row(
                    "account",
                    cap,
                    false,
                    "UNVERIFIED: this host cannot reach the probe address, so there is no control",
                    "connect",
                ),
            },
            _ => row(
                "account",
                cap,
                false,
                "no probe for this capability yet",
                "",
            ),
        })
        .collect()
}

/// The `account` rows: measured where the setup is applied, else the committed ones carried over.
fn account_rows(probe: &Path, committed: &str) -> Vec<Row> {
    // `--no-privileged` forces the carry-over path, which is what a machine without the setup (CI) takes.
    #[cfg(windows)]
    if !std::env::args().any(|a| a == "--no-privileged") && antspawn::probes::account::set_up() {
        println!("account: set up here, running its probes");
        return account_measured(probe);
    }
    let _ = probe;
    carry_over("account", committed)
}

/// The committed rows of a launcher this machine cannot measure, if they are whole and sound.
fn carry_over(launcher: &str, committed: &str) -> Vec<Row> {
    let kept: Vec<Row> = Matrix::parse(committed)
        .map(|m| {
            m.rows()
                .iter()
                .filter(|r| r.backend == launcher)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let complete = Capability::ALL
        .iter()
        .all(|c| kept.iter().filter(|r| r.subject == c.id()).count() == 1);
    // A carried-over `yes` must name the probe that earned it; anything else is rebuilt as UNVERIFIED.
    let sound = kept
        .iter()
        .all(|r| !r.enabled || r.metadata.starts_with("probe="));
    if complete && sound && kept.len() == Capability::ALL.len() {
        println!("{launcher}: not measured here, carrying over the committed rows");
        kept
    } else {
        unverified(launcher)
    }
}

fn probe_exe() -> PathBuf {
    let exe = std::env::current_exe().expect("own path");
    // target/<profile>/examples/matrix -> target/<profile>/spawn-probe
    let dir = exe.parent().and_then(|p| p.parent()).expect("target dir");
    dir.join(format!("spawn-probe{}", std::env::consts::EXE_SUFFIX))
}

fn render(rows: &[Row]) -> String {
    let mut out = format!("{HEADER}\n");
    for r in rows {
        let enabled = if r.enabled { "yes" } else { "no" };
        let fields = [
            r.subject_kind.as_str(),
            &r.subject,
            &r.backend,
            &r.version,
            &r.config,
            enabled,
            &r.reason,
            &r.metadata,
            &r.note,
        ];
        out.push_str(&fields.join("\t"));
        out.push('\n');
    }
    out
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
    let probes = Probes::new(exe.clone());

    let committed = std::fs::read_to_string(FILE).unwrap_or_default();
    let mut rows = naive(&probes);
    rows.extend(account_rows(&exe, &committed));
    // No probe run drives the sandbox yet (it needs an interactive desktop): never `yes` from here.
    rows.extend(carry_over("windows-sandbox", &committed));
    let text = render(&rows);
    // Whatever was generated must be accepted by the reader that will certify launchers from it.
    let parsed = Matrix::parse(&text).expect("generated matrix parses");
    assert_eq!(parsed.to_tsv(), text, "generated text is canonical");

    if check {
        if committed.replace("\r\n", "\n") == text {
            println!("matrix/launchers.tsv is up to date");
            ExitCode::SUCCESS
        } else {
            eprintln!("matrix/launchers.tsv differs from the generated matrix:\n{text}");
            ExitCode::FAILURE
        }
    } else {
        std::fs::write(FILE, &text).expect("write matrix");
        println!("wrote {FILE}");
        ExitCode::SUCCESS
    }
}
