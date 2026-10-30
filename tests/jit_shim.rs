// SPDX-License-Identifier: MIT OR Apache-2.0
//! The runner hand-off (board 260): the JIT config reaches the runner's environment through the
//! shim's stdin, and only there. The "runner" is a shell that exits 0 only if the variable holds the
//! expected value, so the value itself is never printed.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use antspawn::adapter::github::Secret;
use antspawn::adapter::handoff::start_shim;

const SHIM: &str = env!("CARGO_BIN_EXE_spawn-jit-shim");
const VAR: &str = "ACTIONS_RUNNER_INPUT_JITCONFIG";

/// `(runner, args)` that exit 0 iff `$VAR` equals `want` (3 otherwise, 4 if it is unset).
fn expects(want: &str) -> (&'static str, Vec<OsString>) {
    if cfg!(windows) {
        (
            "cmd",
            vec![
                "/c".into(),
                format!(
                    "if not defined {VAR} (exit 4) else if \"%{VAR}%\"==\"{want}\" (exit 0) else (exit 3)"
                )
                .into(),
            ],
        )
    } else {
        (
            "sh",
            vec![
                "-c".into(),
                format!(
                    "[ -z \"${VAR}\" ] && exit 4; [ \"${VAR}\" = \"{want}\" ] && exit 0; exit 3"
                )
                .into(),
            ],
        )
    }
}

fn through_shim(jit: &str, want: &str) -> i32 {
    let (runner, args) = expects(want);
    start_shim(
        Path::new(SHIM),
        Path::new(runner),
        &args,
        &Secret::new(jit.to_owned()),
    )
    .expect("shim starts")
    .wait()
    .expect("shim ends")
    .code()
    .expect("exit code")
}

#[test]
fn the_runner_child_sees_the_config_and_this_process_does_not() {
    assert_eq!(through_shim("c2VjcmV0Cg==", "c2VjcmV0Cg=="), 0);
    assert!(
        std::env::var_os(VAR).is_none(),
        "the host's own environment never holds it"
    );
}

#[test]
fn control_a_wrong_value_fails_the_same_check() {
    assert_eq!(
        through_shim("c2VjcmV0Cg==", "b3RoZXI="),
        3,
        "the check can tell values apart"
    );
}

#[test]
fn control_without_the_shim_the_variable_is_unset() {
    let (runner, args) = expects("c2VjcmV0Cg==");
    let status = Command::new(runner)
        .args(args)
        .env_remove(VAR)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(4), "the check can tell unset apart");
}

fn run_shim_with_stdin(bytes: &[u8]) -> (Option<i32>, String) {
    let (runner, args) = expects("x");
    let mut child = Command::new(SHIM)
        .arg(runner)
        .args(args)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(bytes);
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_config_that_is_not_one_is_refused_before_the_runner_starts() {
    // Exit 2 is the shim refusing; the runner stand-in would have exited 3 or 4.
    for bad in [
        &b""[..],
        b"\n",
        b"not base64 !",
        b"has\0nul",
        &[0xff, 0xfe],
        &vec![b'A'; 70_000],
    ] {
        let (code, stderr) = run_shim_with_stdin(bad);
        assert_eq!(code, Some(2), "input {:?}...", &bad[..bad.len().min(8)]);
        assert!(!stderr.contains("AAAA"), "the message never echoes input");
    }
}

#[test]
fn a_missing_runner_is_an_error_that_names_no_secret() {
    let mut child = Command::new(SHIM)
        .arg("definitely-not-a-program-antspawn")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"c2VjcmV0Cg==")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("c2VjcmV0"));
}

// ---- listen mode (board 322, ruled A): exactly one connection, then nothing left to connect to ----

use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::Child;
use std::time::{Duration, Instant};

/// Start the shim in listen mode on a free loopback port; returns the child and the bound address.
fn listening_shim(want: &str) -> (Child, SocketAddr) {
    let (runner, args) = expects(want);
    let mut child = Command::new(SHIM)
        .args(["--listen", "127.0.0.1:0"])
        .arg(runner)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .trim()
        .rsplit(' ')
        .next()
        .and_then(|a| a.parse().ok())
        .unwrap_or_else(|| panic!("no address in {line:?}"));
    (child, addr)
}

fn send_once(addr: SocketAddr, bytes: &[u8]) {
    let mut s = TcpStream::connect(addr).expect("first connection is accepted");
    s.write_all(bytes).unwrap();
}

#[test]
fn listen_the_runner_child_sees_the_config_sent_once() {
    let (child, addr) = listening_shim("c2VjcmV0Cg==");
    send_once(addr, b"c2VjcmV0Cg==");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("c2VjcmV0"));
    assert!(std::env::var_os(VAR).is_none());
}

#[test]
fn listen_control_a_wrong_value_fails_the_same_check() {
    let (child, addr) = listening_shim("b3RoZXI=");
    send_once(addr, b"c2VjcmV0Cg==");
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(3));
}

#[test]
fn listen_a_connection_after_the_first_is_refused_and_the_listener_has_exited() {
    let (mut child, addr) = listening_shim("c2VjcmV0Cg==");
    send_once(addr, b"c2VjcmV0Cg==");
    assert_eq!(child.wait().unwrap().code(), Some(0), "the shim exited");
    assert!(
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_err(),
        "nothing listens after the one connection"
    );
}

#[test]
fn listen_a_second_connection_while_the_first_is_open_is_refused_and_never_read() {
    let (child, addr) = listening_shim("c2VjcmV0Cg==");
    let mut first = TcpStream::connect(addr).unwrap();
    // The shim closes its listener as soon as it has accepted `first`; a second connection that slips
    // into the backlog before that is reset, one after it is refused. Either way it gets no service.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
            Err(_) => break,
            Ok(mut second) => {
                let _ = second.write_all(b"b3RoZXI=");
                second
                    .set_read_timeout(Some(Duration::from_millis(200)))
                    .ok();
                assert!(Instant::now() < deadline, "second connection stayed open");
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    first.write_all(b"c2VjcmV0Cg==").unwrap();
    drop(first);
    // Exit 0 means the runner saw the FIRST connection's value, not the second's.
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(0));
}

#[test]
fn listen_a_config_that_is_not_one_is_refused_before_the_runner_starts() {
    let (child, addr) = listening_shim("x");
    send_once(addr, b"not base64 !");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("not base64 !"));
}

#[test]
fn control_a_naive_listener_would_accept_a_second_connection() {
    // Proves the "second connection is refused" checks above can fail: a listener that keeps
    // listening serves both.
    let naive = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = naive.local_addr().unwrap();
    assert!(TcpStream::connect(addr).is_ok());
    assert!(TcpStream::connect(addr).is_ok());
}

#[test]
fn listen_needs_an_address() {
    let out = Command::new(SHIM)
        .args(["--listen", "not-an-address", "x"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
