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
    assert_eq!(out.status.code(), Some(125));
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
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no launcher is certified"));
}

fn run_with_results(
    tag: &str,
    extra: &str,
    args: &[&str],
) -> (Option<i32>, String, serde_json::Value) {
    let spec = spec_file(tag, extra);
    let results =
        std::env::temp_dir().join(format!("antspawn-cli-res-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&results);
    let out = Command::new(env!("CARGO_BIN_EXE_spawn"))
        .arg("run")
        .arg(&spec)
        .args(args)
        .arg("--results")
        .arg(&results)
        .output()
        .unwrap();
    let doc = std::fs::read_to_string(results.join("spawn-result.json")).unwrap_or_default();
    let _ = std::fs::remove_file(spec);
    let _ = std::fs::remove_dir_all(&results);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        serde_json::from_str(&doc).unwrap_or(serde_json::Value::Null),
    )
}

#[test]
fn a_refusal_exits_125_and_says_refused_in_spawn_result_json() {
    let (code, _, doc) = run_with_results("refuse", "[needs]\nrequire = [\"gpu.cuda\"]\n", &[]);
    assert_eq!(code, Some(125));
    assert_eq!(doc["status"], "refused");
    assert!(doc["exit_code"].is_null());
    assert!(
        doc["message"]
            .as_str()
            .unwrap()
            .contains("no launcher is certified")
    );
}

#[test]
fn an_uncertified_named_launcher_is_also_a_refusal() {
    let (code, _, doc) = run_with_results("named", "", &["--launcher", "naive"]);
    assert_eq!(code, Some(125));
    assert_eq!(doc["status"], "refused");
    assert_eq!(doc["launcher"], "naive");
}

#[test]
fn control_a_bad_spec_is_an_error_not_a_refusal() {
    let spec = std::env::temp_dir().join(format!("antspawn-cli-bad-{}.toml", std::process::id()));
    std::fs::write(&spec, "this is not a job").unwrap();
    let results = std::env::temp_dir().join(format!("antspawn-cli-res-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&results);
    let out = Command::new(env!("CARGO_BIN_EXE_spawn"))
        .arg("run")
        .arg(&spec)
        .arg("--results")
        .arg(&results)
        .output()
        .unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(results.join("spawn-result.json")).unwrap())
            .unwrap();
    let _ = std::fs::remove_file(spec);
    let _ = std::fs::remove_dir_all(&results);
    assert_eq!(out.status.code(), Some(126));
    assert_eq!(doc["status"], "error");
}

#[test]
fn an_output_named_like_a_result_file_is_refused_as_a_bad_spec() {
    let (code, stderr, _) = run_with_results(
        "reserved",
        "[[output]]\nguest = \"o\"\nname = \"stdout.tail\"\n",
        &[],
    );
    assert_eq!(code, Some(126));
    assert!(stderr.contains("reserved"), "{stderr}");
}
