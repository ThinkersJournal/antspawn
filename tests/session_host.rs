// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-runner session` against a real named pipe, run AS THE CURRENT USER in a temporary layout
//! (slice S3 of `docs/SESSION-PLAN.md`). No restricted account is involved, so this runs on every
//! Windows leg; `tests/account_session.rs` runs the same host as the real account (runner-only).
//!
//! What it shows: the pipe admits the SID named in `session.toml` and nobody else (a control with the
//! right SID connects, one with another SID is refused), only one client gets in, state persists across
//! execs, `close` and a plain disconnect both end the host, and a descendant the session left behind
//! dies with the host's Job Object. NOT shown: a remote client being refused (needs a second machine).
#![cfg(windows)]

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use antspawn::session::ExecRequest;
use antspawn::session_proto::{Request, Response, Status, read_frame, write_frame};

const RUNNER: &str = env!("CARGO_BIN_EXE_spawn-runner");
const NONCE: &str = "0123456789abcdef0123456789abcdef";

fn my_sid() -> String {
    let out = Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .rsplit(',')
        .next()
        .unwrap()
        .trim_matches('"')
        .to_owned()
}

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A layout with a `runner.toml` and the session `sid` spooled for `allowed`.
fn layout(tag: &str, sid: &str, allowed: &str) -> Root {
    let root = std::env::temp_dir().join(format!("antspawn-sh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("spool").join(sid)).unwrap();
    std::fs::create_dir_all(root.join("results").join(sid)).unwrap();
    std::fs::write(
        root.join("runner.toml"),
        "max_time_s = 30\nmax_ram_mb = 512\n",
    )
    .unwrap();
    std::fs::write(
        root.join("spool").join(sid).join("session.toml"),
        format!("launcher_sid = \"{allowed}\"\nnonce = \"{NONCE}\"\n"),
    )
    .unwrap();
    Root(root)
}

fn host(root: &Root, sid: &str) -> Child {
    Command::new(RUNNER)
        .args(["session", sid])
        .env("ANTSPAWN_ROOT", &root.0)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn pipe_path(sid: &str) -> String {
    format!(r"\\.\pipe\antspawn-session-{sid}-{NONCE}")
}

/// Open the pipe, retrying while the host has not created it yet (not found / busy).
fn connect(sid: &str) -> std::io::Result<File> {
    let start = Instant::now();
    loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_path(sid))
        {
            Err(e)
                if matches!(e.raw_os_error(), Some(2 | 231))
                    && start.elapsed() < Duration::from_secs(20) =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

fn exec(pipe: &mut File, id: u64, cmd: &[&str]) -> Response {
    let req = ExecRequest::new(cmd.iter().copied());
    write_frame(pipe, &Request::exec(id, &req)).unwrap();
    read_frame::<_, Response>(pipe).unwrap().expect("an answer")
}

fn stdout_of(r: &Response) -> String {
    match r {
        Response::Exec { stdout, .. } => {
            use base64::Engine;
            let b = base64::engine::general_purpose::STANDARD
                .decode(stdout)
                .unwrap();
            String::from_utf8_lossy(&b).into_owned()
        }
        other => panic!("not an exec answer: {other:?}"),
    }
}

fn wait_exit(child: &mut Child, secs: u64) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn ended(root: &Root, sid: &str) -> String {
    std::fs::read_to_string(root.0.join("results").join(sid).join("session.end"))
        .unwrap_or_default()
}

#[test]
fn the_right_sid_connects_runs_commands_keeps_state_and_closes() {
    let sid = "t-ok";
    let root = layout("ok", sid, &my_sid());
    let mut h = host(&root, sid);
    let mut pipe = connect(sid).expect("the allowed SID connects");

    let w = exec(&mut pipe, 1, &["cmd", "/c", "echo kept> f.txt"]);
    assert!(
        matches!(
            w,
            Response::Exec {
                id: 1,
                status: Status::Exited,
                exit_code: Some(0),
                ..
            }
        ),
        "{w:?}"
    );
    let r = exec(&mut pipe, 2, &["cmd", "/c", "type f.txt"]);
    assert!(
        stdout_of(&r).contains("kept"),
        "state did not persist: {r:?}"
    );
    // The work dir is the host's, under the layout, not the test's cwd.
    assert!(root.0.join("work").join(sid).join("f.txt").is_file());

    write_frame(&mut pipe, &Request::Close {}).unwrap();
    assert_eq!(
        read_frame::<_, Response>(&mut pipe).unwrap(),
        Some(Response::Closed {})
    );
    assert!(wait_exit(&mut h, 20), "the host did not exit after close");
    assert_eq!(ended(&root, sid), "closed");
}

#[test]
fn a_different_sid_is_refused_and_a_second_client_finds_the_pipe_taken() {
    // Control for the refusal: the same layout with MY sid connects (test above). Here the ACE names
    // LocalService (S-1-5-19), which this process is not.
    let sid = "t-other";
    let root = layout("other", sid, "S-1-5-19");
    let mut h = host(&root, sid);
    // Wait for the pipe to exist (the host creates it first thing), then try as ourselves.
    let start = Instant::now();
    let denied = loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_path(sid))
        {
            Err(e) if e.kind() == ErrorKind::PermissionDenied => break true,
            Err(e)
                if matches!(e.raw_os_error(), Some(2 | 231))
                    && start.elapsed() < Duration::from_secs(20) =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(_) => break false,
            Err(e) => panic!("unexpected error {e}"),
        }
    };
    let _ = h.kill();
    let _ = h.wait();
    assert!(
        denied,
        "a user the DACL does not name was able to open the pipe"
    );
}

#[test]
fn only_one_client_gets_in() {
    let sid = "t-one";
    let root = layout("one", sid, &my_sid());
    let mut h = host(&root, sid);
    let first = connect(sid).expect("first client");
    // The one instance is taken; a second open finds it busy (231), never a second session.
    let second = OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_path(sid));
    assert!(
        matches!(&second, Err(e) if e.raw_os_error() == Some(231)),
        "second client: {second:?}"
    );
    drop(first);
    assert!(wait_exit(&mut h, 20));
    assert_eq!(ended(&root, sid), "disconnected");
}

fn heartbeat_len(dir: &Path) -> u64 {
    std::fs::metadata(dir.join("hb.txt")).map_or(0, |m| m.len())
}

#[test]
fn a_disconnect_ends_the_host_and_a_survivor_dies_with_its_job() {
    let sid = "t-drop";
    let root = layout("drop", sid, &my_sid());
    let mut h = host(&root, sid);
    let mut pipe = connect(sid).unwrap();
    // A survivor: a detached batch file that appends a line a second until killed. (Written by the
    // test into the host's work dir; nested quotes through `cmd /c` did not survive argument escaping.)
    let work = root.0.join("work").join(sid);
    std::fs::write(
        work.join("hb.cmd"),
        "@echo off
for /l %%i in (1,1,200) do (echo x>>hb.txt & ping -n 2 127.0.0.1>nul)
",
    )
    .unwrap();
    let r = exec(&mut pipe, 1, &["cmd", "/c", "start /b hb.cmd"]);
    assert!(
        matches!(
            r,
            Response::Exec {
                status: Status::Exited,
                ..
            }
        ),
        "{r:?}"
    );
    // Control: the survivor really is running and writing while the session is up.
    let a = {
        std::thread::sleep(Duration::from_millis(1500));
        heartbeat_len(&work)
    };
    std::thread::sleep(Duration::from_millis(2500));
    let b = heartbeat_len(&work);
    assert!(
        b > a && a > 0,
        "the survivor never wrote (control failed): {a} -> {b}"
    );

    // The owner vanishes without a close frame.
    drop(pipe);
    assert!(wait_exit(&mut h, 20), "the host outlived its client");
    assert_eq!(ended(&root, sid), "disconnected");
    let c = heartbeat_len(&work);
    std::thread::sleep(Duration::from_millis(3000));
    let d = heartbeat_len(&work);
    assert_eq!(c, d, "the survivor kept writing after the host's job ended");
}
