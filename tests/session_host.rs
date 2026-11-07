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

/// The SID the host will accept for a session this process spools: the OWNER of what it creates. For a
/// normal user that is the user's own SID; for an elevated administrator Windows makes the owner
/// `Administrators` (S-1-5-32-544), which the token carries too, so the same test runs on both.
fn my_sid() -> String {
    let probe = std::env::temp_dir().join(format!(
        "antspawn-sh-ownerprobe-{}-{:?}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&probe, b"x").unwrap();
    let sid = antspawn::session_pipe::owner_sid(&probe).expect("the owner of a file we made");
    let _ = std::fs::remove_file(&probe);
    sid
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
fn a_session_naming_someone_who_does_not_own_the_spool_entry_is_refused_before_any_pipe_exists() {
    // The security point of the owner check: the DACL admits `launcher_sid`, so the host must not take that
    // from the file alone. Here the file names LocalService (S-1-5-19) but is owned by us.
    // Control: the same layout naming OUR owner SID connects (`the_right_sid_connects_...`).
    let sid = "t-owner";
    let root = layout("owner", sid, "S-1-5-19");
    let mut h = host(&root, sid);
    assert!(wait_exit(&mut h, 20), "the host did not refuse");
    assert!(!h.wait().unwrap().success(), "a refusal must exit non-zero");
    let why = std::fs::read_to_string(root.0.join("results").join(sid).join("error.txt"))
        .unwrap_or_default();
    assert!(why.contains("not the owner of the spool entry"), "{why:?}");
    let pipe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_path(sid));
    assert!(
        matches!(&pipe, Err(e) if e.raw_os_error() == Some(2)),
        "no pipe may have been created: {pipe:?}"
    );
}

#[test]
fn the_pipe_dacl_admits_only_the_named_sid() {
    // The DACL itself, in-process, with a SID that is not ours (LocalService). Control: the owner SID connects.
    let (denied_name, ok_name) = (
        format!("antspawn-dacl-deny-{}", std::process::id()),
        format!("antspawn-dacl-ok-{}", std::process::id()),
    );
    let (n1, n2, mine) = (denied_name.clone(), ok_name.clone(), my_sid());
    std::thread::spawn(move || {
        let _ = antspawn::session_pipe::accept(&n1, "S-1-5-19");
    });
    std::thread::spawn(move || {
        let _ = antspawn::session_pipe::accept(&n2, &mine);
    });
    let start = Instant::now();
    let open = |n: &str| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!(r"\\.\pipe\{n}"))
    };
    let mut denied = None;
    while denied.is_none() && start.elapsed() < Duration::from_secs(20) {
        match open(&denied_name) {
            Err(e) if e.kind() == ErrorKind::PermissionDenied => denied = Some(true),
            Err(e) if matches!(e.raw_os_error(), Some(2 | 231)) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(_) => denied = Some(false),
            Err(e) => panic!("unexpected error {e}"),
        }
    }
    assert_eq!(
        denied,
        Some(true),
        "a user the DACL does not name was able to open the pipe"
    );
    let mut ok = open(&ok_name);
    while matches!(&ok, Err(e) if matches!(e.raw_os_error(), Some(2 | 231)))
        && start.elapsed() < Duration::from_secs(20)
    {
        std::thread::sleep(Duration::from_millis(50));
        ok = open(&ok_name);
    }
    assert!(ok.is_ok(), "the named SID must connect: {ok:?}");
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
    // Poll for growth rather than sleep a fixed 2.5 s: under a loaded box (other tests in this binary start
    // processes too) a one-second ping can lag, and a fixed wait failed this control 2 runs in 5.
    let mut b = heartbeat_len(&work);
    let poll = Instant::now();
    while b <= a && poll.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(250));
        b = heartbeat_len(&work);
    }
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

#[test]
fn a_session_ended_by_a_broken_frame_does_not_publish_exit_code_zero() {
    // Pip review of #74: `session.end` said `broken` while the result still decoded as a success. Control: a
    // clean close publishes 0 (the first test also asserts `session.end` = closed).
    use antspawn::launcher::Exit;
    use antspawn::runner::decode_exit;
    use std::io::Write;
    let sid = "t-broken";
    let root = layout("broken", sid, &my_sid());
    let mut h = host(&root, sid);
    let mut pipe = connect(sid).unwrap();
    // A length above the 1 MiB cap: the stream is out of step and the host ends the session.
    pipe.write_all(&u32::MAX.to_le_bytes()).unwrap();
    assert!(wait_exit(&mut h, 20), "the host did not end");
    assert!(
        ended(&root, sid).starts_with("broken"),
        "{:?}",
        ended(&root, sid)
    );
    let tmp = std::fs::read_to_string(root.0.join("results").join(sid).join("result.tmp")).unwrap();
    assert_eq!(decode_exit(&tmp), Some(Exit::Killed), "{tmp:?}");

    let sid = "t-clean";
    let root = layout("clean", sid, &my_sid());
    let mut h = host(&root, sid);
    let mut pipe = connect(sid).unwrap();
    write_frame(&mut pipe, &Request::Close {}).unwrap();
    let _ = read_frame::<_, Response>(&mut pipe);
    assert!(wait_exit(&mut h, 20));
    let tmp = std::fs::read_to_string(root.0.join("results").join(sid).join("result.tmp")).unwrap();
    assert_eq!(decode_exit(&tmp), Some(Exit::Code(0)), "{tmp:?}");
}

#[test]
fn a_work_dir_that_cannot_be_wiped_refuses_the_session_instead_of_leaking_into_it() {
    // A file held open without sharing cannot be deleted, so the wipe of the previous contents fails.
    // Control: with nothing held the same layout starts (the tests above).
    use std::os::windows::fs::OpenOptionsExt;
    let sid = "t-wipe";
    let root = layout("wipe", sid, &my_sid());
    let work = root.0.join("work").join(sid);
    std::fs::create_dir_all(&work).unwrap();
    let held = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0)
        .open(work.join("held.txt"))
        .unwrap();
    let mut h = host(&root, sid);
    assert!(
        wait_exit(&mut h, 20),
        "the host started a session over a work dir it could not wipe"
    );
    assert!(!h.wait().unwrap().success());
    let why = std::fs::read_to_string(root.0.join("results").join(sid).join("error.txt"))
        .unwrap_or_default();
    assert!(
        why.contains("could not remove the previous contents"),
        "{why:?}"
    );
    drop(held);
}
