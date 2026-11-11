// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-sandbox-broker --check`, the real binary, on real files: a good config is accepted and
//! described, a bad one is refused with exit 1 naming the field, a missing file and bad usage are
//! refused. Starts no sandbox and no server. NOT shown: serving.

use std::process::Command;

const GOOD: &str = r#"
pipe = "antspawn-sandbox-broker"
allow_sids = ["S-1-5-21-1-2-3-1001"]

[[profiles]]
name = "default"
template = 'C:\antspawn\sandbox\default.wsb'
"#;

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_spawn-sandbox-broker"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn file(tag: &str, text: &str) -> String {
    let p = std::env::temp_dir().join(format!("antspawn-sbc-{tag}-{}.toml", std::process::id()));
    std::fs::write(&p, text).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn a_good_config_is_accepted_and_described() {
    let f = file("good", GOOD);
    let (code, out, err) = run(&["--config", &f, "--check"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("config ok") && out.contains("default"),
        "{out}"
    );
    let _ = std::fs::remove_file(f);
}

#[test]
fn a_bad_config_is_refused_naming_the_field() {
    let f = file("bad", &GOOD.replace(".wsb", ".exe"));
    let (code, out, err) = run(&["--config", &f, "--check"]);
    assert_eq!(code, 1);
    assert!(err.contains("template") && out.is_empty(), "{err} / {out}");
    let _ = std::fs::remove_file(f);
}

#[test]
fn a_missing_file_and_bad_usage_are_refused() {
    let (code, _, err) = run(&["--config", "no-such-file.toml", "--check"]);
    assert_eq!(code, 1);
    assert!(err.contains("cannot read"), "{err}");
    assert_eq!(run(&[]).0, 2);
    assert_eq!(run(&["--check"]).0, 2);
    assert_eq!(run(&["--bogus"]).0, 2);
}
