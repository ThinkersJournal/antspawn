// SPDX-License-Identifier: MIT OR Apache-2.0
//! The broker's real pipe, in-process (slice V3). Shown: the SID read back is the connecting account's,
//! two clients both get through, the name cannot be taken twice, and a DACL that admits someone else
//! refuses this account. NOT shown: a client from a different account.
#![cfg(windows)]

use antspawn::broker_pipe::Listener;
use std::fs::OpenOptions;
use std::io::{Read, Write};

/// This account's SID from `whoami` (the file owner is the Administrators group on an elevated CI
/// runner, which is not the token's user and so not what the broker reads).
fn me() -> String {
    let out = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let sid = text.trim().rsplit(',').next().unwrap().trim_matches('"');
    assert!(
        antspawn::session_host::valid_sid(sid),
        "whoami said {text:?}"
    );
    sid.to_string()
}

fn name(tag: &str) -> String {
    format!("antspawn-test-{tag}-{}", std::process::id())
}

fn open(n: &str) -> std::io::Result<std::fs::File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!(r"\\.\pipe\{n}"))
}

#[test]
fn the_sid_read_back_is_the_connecting_accounts_and_bytes_flow() {
    let n = name("sid");
    let sid = me();
    let mut l = Listener::new(&n, std::slice::from_ref(&sid)).unwrap();
    let n2 = n.clone();
    let t = std::thread::spawn(move || {
        let mut c = open(&n2).unwrap();
        c.write_all(b"ping").unwrap();
        let mut b = [0u8; 4];
        c.read_exact(&mut b).unwrap();
        b
    });
    let (mut p, who) = l.accept().unwrap();
    assert!(who.eq_ignore_ascii_case(&sid), "{who} != {sid}");
    let mut b = [0u8; 4];
    p.read_exact(&mut b).unwrap();
    assert_eq!(&b, b"ping");
    p.write_all(b"pong").unwrap();
    assert_eq!(&t.join().unwrap(), b"pong");
}

#[test]
fn two_clients_both_connect_and_neither_waits_for_the_other_to_finish() {
    let n = name("two");
    let mut l = Listener::new(&n, &[me()]).unwrap();
    // Only one instance exists before the first accept; a second client would find it busy. Once the
    // first accept returns, the next instance already exists, so B connects while A is still open.
    let a = open(&n).unwrap();
    let (_p1, s1) = l.accept().unwrap();
    let b = open(&n).expect("the next instance must exist before accept returns");
    let (_p2, s2) = l.accept().unwrap();
    assert_eq!(s1, s2);
    drop((a, b));
}

#[test]
fn the_name_cannot_be_taken_twice() {
    let n = name("dup");
    let _first = Listener::new(&n, &[me()]).unwrap();
    assert!(Listener::new(&n, &[me()]).is_err());
}

#[test]
fn a_dacl_that_admits_someone_else_refuses_this_account() {
    let n = name("deny");
    // SYSTEM only: the known-bad control for the allow list. This test process is not SYSTEM.
    let _l = Listener::new(&n, &["S-1-5-18".to_string()]).unwrap();
    let e = open(&n).expect_err("an account not on the list got in");
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
}

#[test]
fn a_bad_name_or_sid_is_refused() {
    assert!(Listener::new("a/b", &[me()]).is_err());
    assert!(Listener::new("", &[me()]).is_err());
    assert!(Listener::new("ok", &[]).is_err());
    assert!(Listener::new("ok", &["S-1-5".to_string()]).is_err());
    assert!(Listener::new("ok", &["S-1-5-18)(A;;GA;;;WD".to_string()]).is_err());
}
