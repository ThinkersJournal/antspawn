// SPDX-License-Identifier: MIT OR Apache-2.0
//! The platform-independent half of a session host (slice S3 of `docs/SESSION-PLAN.md`).
//!
//! `spawn-runner session <sid>` owns the pipe, the DACL and the Job Object (all Win32, in the binary);
//! what it does with a connected peer is here, over any `Read + Write`, so every CI leg runs it:
//!
//! * [`serve_session`]: read frames, run each `exec` through a closure, answer, stop on `close`,
//!   on the peer disconnecting, or on a frame that leaves the stream out of step;
//! * [`run_command`]: run one command in the session's work dir with a time limit and stream tails;
//! * [`SessionFile`]: the `spool/<sid>/session.toml` the launcher writes and the host trusts only
//!   after validating (the host does not trust the launcher, nor the launcher the host).

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::launcher::Exit;
use crate::runner::safe_relative;
use crate::session::{ExecOutcome, ExecRequest};
use crate::session_files::WorkFiles;
use crate::session_proto::{ProtoError, Request, Response, read_frame, write_frame};
use crate::streams::{Streams, Tail, settle};

/// What the launcher tells the host before the host exists: who may connect and the pipe's nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFile {
    /// SID of the launcher's user: the only principal the pipe's DACL admits.
    pub launcher_sid: String,
    /// Unguessable suffix of the pipe name, so the name is not predictable from the session id.
    pub nonce: String,
    /// `bind` workspace (slice W4): the real project folder the launcher granted the account. Commands
    /// run there instead of the host's own work dir. Absent: the session has its own work dir.
    /// An older host rejects the field (`deny_unknown_fields`), so a bind never silently runs in an
    /// empty directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<std::path::PathBuf>,
}

/// The folder a `bind` session runs in, or why the host refuses it: an absolute plain drive path
/// that is an existing directory and does not overlap the runner's own spool, results or work dirs
/// (a bind there would hand the account another session's files). The host never deletes it.
pub fn bind_root(
    root: &Path,
    layout: &crate::runner::Layout,
) -> Result<std::path::PathBuf, String> {
    use std::path::{Component, Prefix};
    let drive = matches!(root.components().next(),
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_)));
    let plain = root.is_absolute()
        && drive
        && root
            .components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir));
    if !plain {
        return Err(format!(
            "session.toml: root `{}` must be an absolute drive path without `.` or `..`",
            root.display()
        ));
    }
    if !root.is_dir() {
        return Err(format!(
            "session.toml: root `{}` is not a directory",
            root.display()
        ));
    }
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    let r = norm(root);
    for own in [
        layout.spool(),
        layout.results(),
        layout.work(),
        layout.root.clone(),
    ] {
        let o = norm(&own);
        if r == o || r.starts_with(&format!("{o}/")) || o.starts_with(&format!("{r}/")) {
            return Err(format!(
                "session.toml: root `{}` overlaps the runner's own folder `{}`; refused",
                root.display(),
                own.display()
            ));
        }
    }
    Ok(root.to_path_buf())
}

/// `S-1-<authority>-<sub-authority>...`: every part after `S-1-` is a non-empty run of digits, an
/// authority and at least one sub-authority, at most 15 of the latter, 184 bytes at most. The only
/// shape that may reach an SDDL string or a pipe path.
#[must_use]
pub fn valid_sid(sid: &str) -> bool {
    sid.len() <= 184
        && sid.strip_prefix("S-1-").is_some_and(|rest| {
            let parts: Vec<&str> = rest.split('-').collect();
            (2..=16).contains(&parts.len())
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
}

impl SessionFile {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let f: SessionFile = toml::from_str(text).map_err(|e| format!("session.toml: {e}"))?;
        f.validate()?;
        Ok(f)
    }

    /// Both values end up in an SDDL string and a pipe path, so only their plain shapes pass.
    pub fn validate(&self) -> Result<(), String> {
        // `S-1-<authority>-<sub-authority>...`: every part after `S-1-` is a non-empty run of digits,
        // an authority and at least one sub-authority, at most 15 of the latter. (`S-1-` alone, `S-1--`
        // and `S-1-5-` used to pass the shape check and fail later with an unrelated message.)
        let sid_ok = valid_sid(&self.launcher_sid);
        if !sid_ok {
            return Err("session.toml: launcher_sid is not a SID".into());
        }
        let nonce_ok = (16..=64).contains(&self.nonce.len())
            && self.nonce.chars().all(|c| c.is_ascii_hexdigit());
        if !nonce_ok {
            return Err("session.toml: nonce must be 16..=64 hex digits".into());
        }
        Ok(())
    }

    /// The DACL admits `launcher_sid` and nothing else, so the DACL is only as good as this check: the
    /// file is written by the launcher, and a spool that more than one user can write into would let user
    /// A name user B here. The host therefore asks the OS who OWNS the spool entry (`owner`, from
    /// [`crate::session_pipe::owner_sid`]) and refuses a session whose `launcher_sid` is anyone else.
    /// (An elevated launcher creates files owned by `Administrators`; that is refused, loudly, not guessed.)
    pub fn check_owner(&self, owner: &str) -> Result<(), String> {
        if owner.eq_ignore_ascii_case(&self.launcher_sid) {
            Ok(())
        } else {
            Err(format!(
                "session.toml: launcher_sid {} is not the owner of the spool entry ({owner}); refused",
                self.launcher_sid
            ))
        }
    }

    /// `antspawn-session-<sid>-<nonce>`: one path component, no separators. `session_id` must be a valid
    /// job id (the caller checked; this is the backstop so the claim does not rest on that alone).
    ///
    /// # Panics
    /// If `session_id` is not a valid job id.
    pub fn pipe_name(&self, session_id: &str) -> String {
        assert!(
            crate::runner::valid_job_id(session_id),
            "pipe_name: the session id is not a valid job id"
        );
        format!("antspawn-session-{session_id}-{}", self.nonce)
    }
}

/// Why [`serve_session`] returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The peer asked to close and was answered.
    Closed,
    /// The peer went away between frames (including a killed launcher, whose handles the OS closes).
    Disconnected,
    /// A frame left the stream out of step (bad length, truncated) or the pipe failed.
    Broken(String),
}

/// Serve one connected peer until it closes, disconnects or breaks the framing.
///
/// A frame that is intact but not a valid request (bad JSON, unknown op, extra field, invalid exec)
/// is answered with [`Response::Error`] and the session goes on: the stream is still in step. A
/// bad length or a truncated frame is not recoverable, so the session ends.
pub fn serve_session<R: Read, W: Write>(
    r: &mut R,
    w: &mut W,
    files: &WorkFiles,
    mut exec: impl FnMut(&ExecRequest) -> ExecOutcome,
) -> Ended {
    loop {
        let req: Request = match read_frame(r) {
            Ok(Some(req)) => req,
            Ok(None) => return Ended::Disconnected,
            Err(e @ (ProtoError::Malformed(_) | ProtoError::Invalid(_))) => {
                if let Err(e) = reply(w, &error(None, e.to_string())) {
                    return Ended::Broken(e);
                }
                continue;
            }
            Err(e) => return Ended::Broken(e.to_string()),
        };
        if matches!(req, Request::Close {}) {
            return match reply(w, &Response::Closed {}) {
                Ok(()) => Ended::Closed,
                Err(e) => Ended::Broken(e),
            };
        }
        let answer = match req {
            Request::Close {} => unreachable!("returned above"),
            Request::Exec { id, .. } => match req.into_exec() {
                Ok((id, er)) => Response::from_outcome(id, &exec(&er)),
                Err(e) => error(Some(id), e.to_string()),
            },
            other => file_op(files, other),
        };
        if let Err(e) = reply(w, &answer) {
            return Ended::Broken(e);
        }
    }
}

/// Answer a `put`, `get` or `list` against the session's workspace; a refusal is an `error` frame
/// and the session goes on.
fn file_op(files: &WorkFiles, req: Request) -> Response {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    match req {
        Request::Put {
            id,
            path,
            offset,
            data,
        } => match STANDARD.decode(&data) {
            Err(e) => error(Some(id), format!("put: data is not base64: {e}")),
            Ok(bytes) => match files.put(&path, offset, &bytes) {
                Ok(size) => Response::Stored { id, size },
                Err(m) => error(Some(id), format!("put: {m}")),
            },
        },
        Request::Get {
            id,
            path,
            offset,
            len,
        } => match files.get(&path, offset, len) {
            Ok((bytes, eof)) => Response::Data {
                id,
                data: STANDARD.encode(bytes),
                eof,
            },
            Err(m) => error(Some(id), format!("get: {m}")),
        },
        Request::List { id, after } => match files.list(after.as_deref()) {
            Ok((files, more)) => Response::Files { id, files, more },
            Err(m) => error(Some(id), format!("list: {m}")),
        },
        Request::Exec { id, .. } => error(Some(id), "not a file operation".into()),
        Request::Close {} => error(None, "not a file operation".into()),
    }
}

fn error(id: Option<u64>, message: String) -> Response {
    Response::Error { id, message }
}

fn reply<W: Write>(w: &mut W, msg: &Response) -> Result<(), String> {
    write_frame(w, msg).map_err(|e| e.to_string())
}

/// Run `req` in `work`, to completion or its time limit (`default_limit_s` when it names none, and
/// never above `max_limit_s`). A program copied into `work` is found there; anything else is looked
/// up on `PATH`. Descendants that outlive the command are NOT killed here: the session's Job Object
/// owns them until the session ends.
///
/// `scratch` becomes the command's `TEMP` and `TMP` (unless the request sets them itself), so what
/// it writes there is the session's and goes with it, instead of landing in the account's profile
/// temp. `alive` is asked about every 20 ms while the command runs; once it says `false` (the owner
/// is gone) the command is killed and the result is [`Exit::Killed`]: an owner that dies DURING a
/// long exec ends the session then, not after the exec.
pub fn run_command(
    work: &Path,
    scratch: &Path,
    req: &ExecRequest,
    default_limit_s: u64,
    max_limit_s: u64,
    alive: &mut dyn FnMut() -> bool,
) -> ExecOutcome {
    let streams_of = |out: &Tail, err: &Tail| Streams {
        stdout: Some(out.snapshot()),
        stderr: Some(err.snapshot()),
        combined: false,
    };
    let (out_tail, err_tail) = (Tail::new(), Tail::new());
    let cwd = req
        .cwd
        .as_deref()
        .map_or(work.to_path_buf(), |c| work.join(c));
    let limit = Duration::from_secs(req.time_s.unwrap_or(default_limit_s).min(max_limit_s));
    let program = {
        let inside = work.join(&req.cmd[0]);
        if safe_relative(&req.cmd[0]) && inside.is_file() {
            inside
        } else {
            req.cmd[0].clone().into()
        }
    };
    // No scratch dir means TEMP would point nowhere and a program may fall back to the profile temp, which
    // the scratch exists to prevent: do not run the command, say why (126: found but cannot be run).
    if let Err(e) = std::fs::create_dir_all(scratch) {
        err_tail_write(
            &err_tail,
            &format!(
                "not run: cannot create the session scratch dir {}: {e}",
                scratch.display()
            ),
        );
        return ExecOutcome {
            exit: Exit::Code(126),
            streams: streams_of(&out_tail, &err_tail),
        };
    }
    let spawned = Command::new(program)
        .args(&req.cmd[1..])
        .current_dir(cwd)
        .env("TEMP", scratch)
        .env("TMP", scratch)
        .envs(&req.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut readers = Vec::new();
    let exit = match spawned {
        Ok(mut child) => {
            if let Some(p) = child.stdout.take() {
                readers.push(out_tail.capture(p));
            }
            if let Some(p) = child.stderr.take() {
                readers.push(err_tail.capture(p));
            }
            wait_child_while(&mut child, limit, alive)
        }
        Err(e) => {
            // Not an error frame: the command failing to start is a result of the exec, as in a job.
            err_tail_write(&err_tail, &format!("cannot start `{}`: {e}", req.cmd[0]));
            Exit::Code(127)
        }
    };
    settle(&mut readers, Duration::from_millis(500));
    ExecOutcome {
        exit,
        streams: streams_of(&out_tail, &err_tail),
    }
}

fn err_tail_write(tail: &Tail, msg: &str) {
    // `Tail` is fed by readers; a message of our own goes through the same door.
    let _ = tail
        .capture(std::io::Cursor::new(msg.as_bytes().to_vec()))
        .join();
}

/// Wait for `child`; past `limit` kill it and say so.
pub fn wait_child(child: &mut std::process::Child, limit: Duration) -> Exit {
    wait_child_while(child, limit, &mut || true)
}

/// [`wait_child`], and also stop (kill, [`Exit::Killed`]) as soon as `alive` returns `false`.
pub fn wait_child_while(
    child: &mut std::process::Child,
    limit: Duration,
    alive: &mut dyn FnMut() -> bool,
) -> Exit {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.code().map_or(Exit::Killed, Exit::Code),
            Ok(None) => {}
            Err(_) => return Exit::Killed,
        }
        if !alive() {
            let _ = child.kill();
            let _ = child.wait();
            return Exit::Killed;
        }
        if start.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            return Exit::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_proto::Status;
    use std::io::Cursor;

    fn nofiles() -> WorkFiles {
        WorkFiles::new(std::env::temp_dir().join("antspawn-host-nofiles"), 1000)
    }

    fn frame(body: &str) -> Vec<u8> {
        let mut v = (body.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(body.as_bytes());
        v
    }

    fn requests(msgs: &[&str]) -> Cursor<Vec<u8>> {
        Cursor::new(msgs.iter().flat_map(|m| frame(m)).collect())
    }

    fn answers(out: Vec<u8>) -> Vec<Response> {
        let mut c = Cursor::new(out);
        let mut v = Vec::new();
        while let Some(r) = read_frame::<_, Response>(&mut c).unwrap() {
            v.push(r);
        }
        v
    }

    fn ok_outcome() -> ExecOutcome {
        ExecOutcome {
            exit: Exit::Code(0),
            streams: Streams {
                stdout: Some(b"hi".to_vec()),
                stderr: Some(Vec::new()),
                combined: false,
            },
        }
    }

    const EXEC: &str = r#"{"op":"exec","id":1,"cmd":["x"]}"#;

    #[test]
    fn exec_then_close_runs_once_and_answers_in_order() {
        let mut out = Vec::new();
        let mut calls = 0;
        let ended = serve_session(
            &mut requests(&[EXEC, r#"{"op":"close"}"#]),
            &mut out,
            &nofiles(),
            |r| {
                calls += 1;
                assert_eq!(r.cmd, ["x"]);
                ok_outcome()
            },
        );
        assert_eq!(ended, Ended::Closed);
        assert_eq!(calls, 1);
        let a = answers(out);
        assert!(
            matches!(
                &a[0],
                Response::Exec {
                    id: 1,
                    status: Status::Exited,
                    ..
                }
            ),
            "{a:?}"
        );
        assert_eq!(a[1], Response::Closed {});
    }

    #[test]
    fn put_get_and_list_are_served_and_a_bad_path_is_an_error_frame_not_the_end() {
        use base64::Engine;
        let dir = temp_work("files");
        let wf = WorkFiles::new(&dir, 1000);
        let data = base64::engine::general_purpose::STANDARD.encode("hi there");
        let msgs = [
            format!(r#"{{"op":"put","id":1,"path":"d/a.txt","offset":0,"data":"{data}"}}"#),
            r#"{"op":"put","id":2,"path":"../evil","offset":0,"data":""}"#.to_string(),
            r#"{"op":"get","id":3,"path":"d/a.txt","offset":3,"len":100}"#.to_string(),
            r#"{"op":"list","id":4}"#.to_string(),
            r#"{"op":"close"}"#.to_string(),
        ];
        let refs: Vec<&str> = msgs.iter().map(String::as_str).collect();
        let mut out = Vec::new();
        let ended = serve_session(&mut requests(&refs), &mut out, &wf, |_| ok_outcome());
        assert_eq!(ended, Ended::Closed);
        let a = answers(out);
        assert!(
            matches!(a[0], Response::Stored { id: 1, size: 8 }),
            "{:?}",
            a[0]
        );
        assert!(
            matches!(&a[1], Response::Error { id: Some(2), .. }),
            "{:?}",
            a[1]
        );
        match &a[2] {
            Response::Data {
                id: 3,
                data,
                eof: true,
            } => assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap(),
                b"there"
            ),
            other => panic!("{other:?}"),
        }
        match &a[3] {
            Response::Files {
                id: 4,
                files,
                more: false,
            } => {
                assert_eq!(files.len(), 1);
                assert_eq!((files[0].path.as_str(), files[0].size), ("d/a.txt", 8));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(a[4], Response::Closed {}));
        assert!(!dir.parent().unwrap().join("evil").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_peer_that_goes_away_ends_the_session_as_disconnected() {
        // No close frame: the end of the stream is the launcher dying.
        let mut out = Vec::new();
        let ended = serve_session(&mut requests(&[EXEC]), &mut out, &nofiles(), |_| {
            ok_outcome()
        });
        assert_eq!(ended, Ended::Disconnected);
        assert_eq!(answers(out).len(), 1);
    }

    #[test]
    fn intact_but_invalid_frames_are_answered_and_the_session_goes_on() {
        let bad = [
            "not json",
            r#"{"op":"cancel","id":1}"#,
            r#"{"op":"close","x":1}"#,
            r#"{"op":"exec","id":2,"cmd":["x"],"cwd":".."}"#,
            r#"{"op":"exec","id":3,"cmd":[]}"#,
        ];
        let mut msgs = bad.to_vec();
        msgs.push(r#"{"op":"close"}"#);
        let mut out = Vec::new();
        let mut calls = 0;
        let ended = serve_session(&mut requests(&msgs), &mut out, &nofiles(), |_| {
            calls += 1;
            ok_outcome()
        });
        assert_eq!(ended, Ended::Closed);
        assert_eq!(calls, 0, "no invalid request may reach the executor");
        let a = answers(out);
        assert_eq!(a.len(), bad.len() + 1, "{a:?}");
        assert!(
            a[..bad.len()]
                .iter()
                .all(|r| matches!(r, Response::Error { .. })),
            "{a:?}"
        );
        // The invalid exec names its id back.
        assert!(
            matches!(&a[3], Response::Error { id: Some(2), .. }),
            "{a:?}"
        );
    }

    #[test]
    fn a_broken_frame_ends_the_session_without_running_anything() {
        let mut over = (2_000_000u32).to_le_bytes().to_vec();
        over.extend_from_slice(b"{}");
        let mut zero = 0u32.to_le_bytes().to_vec();
        zero.extend_from_slice(b"{}");
        let mut cut = frame(EXEC);
        cut.truncate(cut.len() - 3);
        for (name, bytes) in [("oversized", over), ("zero", zero), ("truncated", cut)] {
            let mut out = Vec::new();
            let ended = serve_session(&mut Cursor::new(bytes), &mut out, &nofiles(), |_| {
                panic!("{name}: reached the executor")
            });
            assert!(matches!(ended, Ended::Broken(_)), "{name}: {ended:?}");
        }
    }

    #[test]
    fn a_sid_that_does_not_own_the_spool_entry_is_refused_and_the_owner_is_accepted() {
        let f = SessionFile {
            launcher_sid: "S-1-5-21-1-2-3-1001".into(),
            nonce: "0123456789abcdef".into(),
            root: None,
        };
        assert!(
            f.check_owner("S-1-5-21-1-2-3-1001").is_ok(),
            "the owner must be accepted"
        );
        assert!(
            f.check_owner("s-1-5-21-1-2-3-1001").is_ok(),
            "SID text is not case-sensitive"
        );
        for other in ["S-1-5-21-1-2-3-1002", "S-1-5-32-544", ""] {
            assert!(
                f.check_owner(other).is_err(),
                "{other:?} is not the launcher"
            );
        }
    }

    #[test]
    #[should_panic(expected = "not a valid job id")]
    fn pipe_name_refuses_an_id_with_a_separator() {
        let f = SessionFile {
            launcher_sid: "S-1-5-21-1".into(),
            nonce: "0123456789abcdef".into(),
            root: None,
        };
        let _ = f.pipe_name("a/b");
    }

    #[test]
    fn session_file_accepts_only_plain_values() {
        let ok = |sid: &str, nonce: &str| {
            SessionFile {
                launcher_sid: sid.into(),
                nonce: nonce.into(),
                root: None,
            }
            .validate()
        };
        let n = "0123456789abcdef";
        assert!(ok("S-1-5-21-1-2-3-1001", n).is_ok());
        for bad in [
            "",
            "S-1-5-21)(A;;GA;;;WD",
            "S-2-1",
            "S-1-5 21",
            "x",
            "S-1-",
            "S-1-5",
            "S-1--",
            "S-1-5-",
            "S-1-5--21",
            "S-1-5-S",
            "S-1-5-21-1-2-3-4-5-6-7-8-9-10-11-12-13-14-15-16",
        ] {
            assert!(ok(bad, n).is_err(), "{bad:?}");
        }
        for bad in ["", "abc", "zzzzzzzzzzzzzzzz", "0123456789abcdef\\..\\x"] {
            assert!(ok("S-1-5-21-1", bad).is_err(), "{bad:?}");
        }
        let f =
            SessionFile::from_toml("launcher_sid = \"S-1-5-21-1\"\nnonce = \"0123456789abcdef\"\n")
                .unwrap();
        assert_eq!(
            f.pipe_name("77-ab"),
            "antspawn-session-77-ab-0123456789abcdef"
        );
        assert!(
            SessionFile::from_toml(
                "launcher_sid = \"S-1-5-1\"\nnonce = \"0123456789abcdef\"\nextra = 1\n"
            )
            .is_err()
        );
    }

    fn shell(script: &str) -> ExecRequest {
        if cfg!(windows) {
            ExecRequest::new(["cmd", "/c", script])
        } else {
            ExecRequest::new(["sh", "-c", script])
        }
    }

    fn scratch_of(work: &Path) -> std::path::PathBuf {
        let mut s = work.as_os_str().to_owned();
        s.push(".tmp");
        s.into()
    }

    fn temp_work(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("antspawn-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn state_written_by_one_command_is_seen_by_the_next() {
        let work = temp_work("state");
        let w = run_command(
            &work,
            &scratch_of(&work),
            &shell("echo kept> f.txt"),
            30,
            30,
            &mut || true,
        );
        assert_eq!(w.exit, Exit::Code(0));
        let r = run_command(
            &work,
            &scratch_of(&work),
            &shell(if cfg!(windows) {
                "type f.txt"
            } else {
                "cat f.txt"
            }),
            30,
            30,
            &mut || true,
        );
        assert_eq!(r.exit, Exit::Code(0));
        assert!(String::from_utf8_lossy(r.streams.stdout.as_deref().unwrap()).contains("kept"));
        // Positive control for the check above: a fresh directory does not have the file.
        let other = temp_work("state-other");
        let r = run_command(
            &other,
            &scratch_of(&other),
            &shell(if cfg!(windows) {
                "type f.txt"
            } else {
                "cat f.txt"
            }),
            30,
            30,
            &mut || true,
        );
        assert_ne!(r.exit, Exit::Code(0));
        let _ = std::fs::remove_dir_all(work);
        let _ = std::fs::remove_dir_all(other);
    }

    #[test]
    fn a_command_past_its_limit_is_reported_timed_out_and_a_missing_program_is_127() {
        let work = temp_work("limit");
        let mut slow = if cfg!(windows) {
            ExecRequest::new(["ping", "-n", "30", "127.0.0.1"])
        } else {
            ExecRequest::new(["sleep", "29"])
        };
        slow.time_s = Some(1);
        let t = Instant::now();
        assert_eq!(
            run_command(&work, &scratch_of(&work), &slow, 30, 30, &mut || true).exit,
            Exit::TimedOut
        );
        // The command would run ~29 s; cut at its 1 s limit it is far under 20 s even when process start
        // and kill are slow on a loaded box (the old 4 s bound against a 5 s command was not).
        assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
        // The ceiling wins over a larger ask.
        slow.time_s = Some(100);
        assert_eq!(
            run_command(&work, &scratch_of(&work), &slow, 30, 1, &mut || true).exit,
            Exit::TimedOut
        );
        let gone = run_command(
            &work,
            &scratch_of(&work),
            &ExecRequest::new(["no-such-program-antspawn"]),
            30,
            30,
            &mut || true,
        );
        assert_eq!(gone.exit, Exit::Code(127));
        assert!(!gone.streams.stderr.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(work);
    }

    #[test]
    fn an_owner_that_dies_during_a_long_exec_ends_it_then_not_after() {
        let work = temp_work("owner");
        let mut slow = if cfg!(windows) {
            ExecRequest::new(["ping", "-n", "30", "127.0.0.1"])
        } else {
            ExecRequest::new(["sleep", "30"])
        };
        slow.time_s = Some(60);
        let begun = Instant::now();
        // The owner is "alive" for 300 ms counted from the first look, which is after the spawn.
        let mut first: Option<Instant> = None;
        let mut ticks = 0;
        let out = run_command(&work, &scratch_of(&work), &slow, 60, 60, &mut || {
            ticks += 1;
            first.get_or_insert_with(Instant::now).elapsed() < Duration::from_millis(300)
        });
        assert_eq!(out.exit, Exit::Killed);
        assert!(
            begun.elapsed() < Duration::from_secs(5),
            "{:?}",
            begun.elapsed()
        );
        assert!(ticks > 1);
        // Control: an owner that stays lets a command run to its own end.
        let ok = run_command(
            &work,
            &scratch_of(&work),
            &shell("exit 0"),
            30,
            30,
            &mut || true,
        );
        assert_eq!(ok.exit, Exit::Code(0));
        let _ = std::fs::remove_dir_all(work);
    }

    #[test]
    fn a_scratch_dir_that_cannot_be_created_stops_the_command_instead_of_running_it_with_a_dead_temp()
     {
        // The scratch path sits under a FILE, so create_dir_all fails. Control: the same command runs with a
        // creatable scratch (the TEMP test below).
        let work = temp_work("scratchfail");
        let blocker = work.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let req = ExecRequest::new(["cmd", "/c", "echo ran> %TEMP%\ran.txt"]);
        let out = run_command(&work, &blocker.join("scratch"), &req, 10, 10, &mut || true);
        assert_eq!(out.exit, Exit::Code(126), "{out:?}");
        let err =
            String::from_utf8_lossy(out.streams.stderr.as_deref().unwrap_or_default()).into_owned();
        assert!(
            err.contains("cannot create the session scratch dir"),
            "{err}"
        );
        assert!(!work.join("ran.txt").exists());
    }

    #[test]
    fn temp_points_at_the_scratch_dir_so_it_goes_with_the_session() {
        let work = temp_work("scratch");
        let scratch = scratch_of(&work);
        let show = shell(if cfg!(windows) {
            "echo %TEMP%"
        } else {
            "echo $TEMP"
        });
        let out = run_command(&work, &scratch, &show, 30, 30, &mut || true);
        let said = String::from_utf8_lossy(out.streams.stdout.as_deref().unwrap()).into_owned();
        assert!(said.contains(&*scratch.to_string_lossy()), "{said}");
        assert!(scratch.is_dir(), "created for the command");
        let _ = std::fs::remove_dir_all(work);
        let _ = std::fs::remove_dir_all(scratch);
    }
}

#[cfg(test)]
mod bind_root_tests {
    use super::*;
    use crate::runner::Layout;
    use std::path::PathBuf;

    // The runner is Windows-only and `bind_root` demands a drive path, so the accept case needs one.
    #[cfg(windows)]
    #[test]
    fn a_bind_root_must_be_a_plain_existing_folder_outside_the_runners_own() {
        let base = std::env::temp_dir().join(format!("antspawn-bindroot-{}", std::process::id()));
        let proj = base.join("proj");
        let layout = Layout::new(base.join("runner"));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(layout.work().join("s1")).unwrap();
        assert_eq!(bind_root(&proj, &layout).unwrap(), proj);
        for (bad, why) in [
            (PathBuf::from("rel/dir"), "relative"),
            (proj.join("..").join("proj"), "dotdot"),
            (base.join("missing"), "missing"),
            (layout.work().join("s1"), "a session's work dir"),
            (layout.work(), "the work dir"),
            (layout.root.clone(), "the runner root"),
            (base.clone(), "an ancestor of the runner root"),
            (PathBuf::from("\\\\server\\share\\p"), "unc"),
        ] {
            assert!(bind_root(&bad, &layout).is_err(), "{why}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_there_is_no_drive_path_so_every_root_is_refused() {
        let layout = Layout::new(std::env::temp_dir().join("antspawn-bindroot-runner"));
        assert!(bind_root(&std::env::temp_dir(), &layout).is_err());
    }

    #[test]
    fn root_round_trips_and_an_unknown_field_is_still_refused() {
        let f = SessionFile {
            launcher_sid: "S-1-5-21-1-2-3-1001".into(),
            nonce: "0123456789abcdef".into(),
            root: Some(PathBuf::from("C:\\Projects\\x")),
        };
        let text = toml::to_string(&f).unwrap();
        assert_eq!(SessionFile::from_toml(&text).unwrap(), f);
        let none = SessionFile { root: None, ..f };
        assert!(!toml::to_string(&none).unwrap().contains("root"));
        let extra = format!("{}\nextra = 1\n", toml::to_string(&none).unwrap());
        assert!(SessionFile::from_toml(&extra).is_err());
    }
}
