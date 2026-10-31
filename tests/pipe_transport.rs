// SPDX-License-Identifier: MIT OR Apache-2.0
//! `PipeTransport` against the real `spawn-jit-shim --pipe` on this machine, as the current user
//! (ROADMAP row 21 slice b). Not run against a second account or Task Scheduler: UNMEASURED.
#![cfg(windows)]

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use antspawn::adapter::account_backend::PipeTransport;
use antspawn::adapter::dispatch::JitTransport;
use antspawn::adapter::github::Secret;

const SHIM: &str = env!("CARGO_BIN_EXE_spawn-jit-shim");
const VAR: &str = "ACTIONS_RUNNER_INPUT_JITCONFIG";

fn my_sid() -> String {
    let out = Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.trim()
        .rsplit(',')
        .next()
        .unwrap()
        .trim_matches('"')
        .to_owned()
}

/// `cmd` that exits 0 iff `$VAR` equals `want`, 3 otherwise.
fn expects(want: &str) -> Vec<OsString> {
    vec![
        "/c".into(),
        format!("if \"%{VAR}%\"==\"{want}\" (exit 0) else (exit 3)").into(),
    ]
}

/// Start the shim on `name` (allowing `sid`), without waiting for it to be ready: the transport's retry
/// window is what covers the gap, as it does after a real task start.
fn start(name: &str, sid: &str, want: &str) -> std::process::Child {
    Command::new(SHIM)
        .args(["--pipe", name, "--allow-sid", sid, "cmd"])
        .args(expects(want))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn name(tag: &str) -> String {
    format!("antspawn-pt-{tag}-{}", std::process::id())
}

#[test]
fn the_transport_delivers_the_config_to_a_shim_that_comes_up_after_it_starts_trying() {
    let n = name("ok");
    let mut shim = start(&n, &my_sid(), "c2VjcmV0Cg==");
    let t = PipeTransport {
        window: Duration::from_secs(10),
    };
    t.deliver(&n, &Secret::new("c2VjcmV0Cg==".into())).unwrap();
    assert_eq!(shim.wait().unwrap().code(), Some(0));
    assert!(std::env::var_os(VAR).is_none());
}

#[test]
fn control_a_wrong_value_fails_the_same_check() {
    let n = name("wrong");
    let mut shim = start(&n, &my_sid(), "b3RoZXI=");
    PipeTransport {
        window: Duration::from_secs(10),
    }
    .deliver(&n, &Secret::new("c2VjcmV0Cg==".into()))
    .unwrap();
    assert_eq!(shim.wait().unwrap().code(), Some(3));
}

#[test]
fn a_principal_not_on_the_access_list_is_refused_and_the_error_names_no_secret() {
    let n = name("deny");
    let mut shim = start(&n, "S-1-5-18", "x");
    // Wait for the pipe to exist so the refusal is the DACL's, not a missing pipe.
    let mut line = String::new();
    BufReader::new(shim.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    let err = PipeTransport {
        window: Duration::from_secs(2),
    }
    .deliver(&n, &Secret::new("c2VjcmV0Cg==".into()))
    .unwrap_err();
    assert!(err.contains("permission denied"), "{err}");
    assert!(!err.contains("c2VjcmV0"), "{err}");
    shim.kill().unwrap();
    shim.wait().unwrap();
}
