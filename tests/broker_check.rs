// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-broker --check`, the real binary, on real files: a good config is accepted and described, a
//! bad one is refused with exit 1 naming the field, a missing file and bad usage are refused. Touches
//! no Hyper-V and starts no server. NOT shown: serving.

use std::process::Command;

const GOOD: &str = r#"
pipe = "antspawn-broker"
allow_sids = ["S-1-5-21-1-2-3-1001"]

[hyperv]
base_dir = 'C:\antspawn\base'
vm_dir = 'C:\antspawn\vms'
[hyperv.base_hashes]
debian-12 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

[[profiles]]
name = "small"
memory_mb = 4096
vcpus = 2
disk_gb = 20

[[slots]]
switch = "antspawn-sw1"
host_address = "10.77.1.1"
guest_address = "10.77.1.2"
"#;

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_spawn-broker"))
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
    let p = std::env::temp_dir().join(format!("antspawn-bc-{tag}-{}.toml", std::process::id()));
    std::fs::write(&p, text).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn a_good_config_is_accepted_and_described() {
    let f = file("good", GOOD);
    let (code, out, err) = run(&["--config", &f, "--check"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("config ok") && out.contains("debian-12"),
        "{out}"
    );
    let _ = std::fs::remove_file(f);
}

#[test]
fn a_bad_config_is_refused_naming_the_field() {
    let f = file("bad", &GOOD.replace("10.77.1.2", "192.168.1.2"));
    let (code, out, err) = run(&["--config", &f, "--check"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("guest_address") && out.is_empty(),
        "{err} / {out}"
    );
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
