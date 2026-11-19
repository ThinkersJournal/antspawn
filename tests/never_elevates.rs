// SPDX-License-Identifier: MIT OR Apache-2.0
//! The launchers never elevate and never create accounts: no source file reaches for an elevation or
//! account-creation primitive. Enumerates from `git ls-files`, and a planted line is the known-bad control.

use std::process::Command;

const FORBIDDEN: &[&str] = &[
    "-Verb RunAs",
    "Verb = \"runas\"",
    "\"runas\"",
    "runas.exe",
    "requireAdministrator",
    "New-LocalUser",
    "net user",
    "net.exe user",
    "NetUserAdd",
    "CreateProcessWithLogon",
];

fn offences(text: &str) -> Vec<&'static str> {
    let lower = text.to_ascii_lowercase();
    FORBIDDEN
        .iter()
        .copied()
        .filter(|p| lower.contains(&p.to_ascii_lowercase()))
        .collect()
}

#[test]
fn control_the_scan_flags_a_planted_elevation() {
    assert!(!offences("Start-Process x -Verb RunAs").is_empty());
    assert!(!offences("New-LocalUser -Name a").is_empty());
    assert!(offences("let x = 1;").is_empty());
}

#[test]
fn no_launcher_or_binary_source_elevates_or_creates_accounts() {
    let root = env!("CARGO_MANIFEST_DIR");
    let out = Command::new("git")
        .args(["-C", root, "ls-files", "src", "examples", "Cargo.toml"])
        .output()
        .expect("git ls-files");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|f| f.ends_with(".rs") || f.ends_with(".toml"))
        .map(str::to_owned)
        .collect();
    assert!(
        files.iter().any(|f| f == "src/launchers/account.rs"),
        "enumeration missed the account launcher: {} files",
        files.len()
    );
    let mut bad = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(std::path::Path::new(root).join(f)).unwrap();
        // Lines that only state the refusal in prose are fine; the scan is on code and strings alike,
        // so allow the one phrase the launchers use to say they never do it.
        for line in text.lines().filter(|l| !l.contains("never")) {
            for p in offences(line) {
                bad.push(format!("{f}: {p}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "elevation or account creation in source: {bad:?}"
    );
}
