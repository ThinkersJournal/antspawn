// SPDX-License-Identifier: MIT OR Apache-2.0
//! Regenerate `matrix/launchers.tsv`: `cargo build --bins && cargo run --example matrix [-- --check]`.
//!
//! Runs the negative probes against every launcher that can run unprivileged (`naive`) and writes
//! one `capability` row per launcher and capability. **Nothing is `yes` without a probe that ran
//! and passed**: a probe that found the isolation broken gives `no` with the probe named, a launcher
//! that is not set up gives `no` with `UNVERIFIED`, and a capability with no probe yet gives `no`.
//! `--check` writes nothing and exits 1 if the committed file differs (what CI runs).

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use antspawn::launchers::{Naive, windows_sandbox::SANDBOX_EXE};
use antspawn::matrix::{HEADER, Matrix, Row};
use antspawn::probes::Probes;
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
            _ => row("naive", cap, false, "no probe for this capability yet", ""),
        });
    }
    let _ = std::fs::remove_dir_all(Naive::work_dir());
    let _ = std::fs::remove_dir_all(dir);
    rows
}

/// A launcher whose privileged setup we do not drive from here: never `yes`.
fn unverified(launcher: &str, set_up: bool) -> Vec<Row> {
    let reason = if set_up {
        "UNVERIFIED: set up, but this generator does not run privileged probes (see tests/)"
    } else {
        "UNVERIFIED: not set up"
    };
    Capability::ALL
        .iter()
        .map(|&cap| row(launcher, cap, false, reason, ""))
        .collect()
}

/// Is the account launcher's scheduled task registered here?
fn account_set_up() -> bool {
    cfg!(windows)
        && Command::new("schtasks")
            .args(["/Query", "/TN", antspawn::runner::TASK_NAME])
            .output()
            .is_ok_and(|o| o.status.success())
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
    let probes = Probes::new(exe);

    let mut rows = naive(&probes);
    rows.extend(unverified("account", account_set_up()));
    rows.extend(unverified(
        "windows-sandbox",
        std::path::Path::new(SANDBOX_EXE).exists(),
    ));
    let text = render(&rows);
    // Whatever was generated must be accepted by the reader that will certify launchers from it.
    let parsed = Matrix::parse(&text).expect("generated matrix parses");
    assert_eq!(parsed.to_tsv(), text, "generated text is canonical");

    let committed = std::fs::read_to_string(FILE).unwrap_or_default();
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
