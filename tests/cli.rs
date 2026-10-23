// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn run` refuses a launcher the matrix does not certify, and says why.

use std::process::Command;

fn spec_file(tag: &str, extra: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("antspawn-cli-{tag}-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        format!("[job]\nname = \"t\"\ncmd = [\"x\"]\n{extra}"),
    )
    .unwrap();
    path
}

#[test]
fn run_refuses_the_uncertified_naive_launcher() {
    let spec = spec_file("naive", "");
    let out = Command::new(env!("CARGO_BIN_EXE_spawn"))
        .args(["run", spec.to_str().unwrap(), "--launcher", "naive"])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(spec);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not certified"), "stderr was: {err}");
    assert!(
        err.contains("isolation.no-read-outside"),
        "stderr was: {err}"
    );
}

#[test]
fn run_without_a_launcher_says_none_is_certified_for_an_unmet_need() {
    // `account` is certified for the default needs now, so ask for something no launcher has.
    let spec = spec_file("auto", "[needs]\nrequire = [\"gpu.cuda\"]\n");
    let out = Command::new(env!("CARGO_BIN_EXE_spawn"))
        .args(["run", spec.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(spec);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no launcher is certified"));
}
