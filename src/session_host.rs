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
}

impl SessionFile {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let f: SessionFile = toml::from_str(text).map_err(|e| format!("session.toml: {e}"))?;
        f.validate()?;
        Ok(f)
    }

    /// Both values end up in an SDDL string and a pipe path, so only their plain shapes pass.
    pub fn validate(&self) -> Result<(), String> {
        let sid_ok = self.launcher_sid.starts_with("S-1-")
            && self.launcher_sid.len() <= 184
            && self
                .launcher_sid
                .chars()
                .all(|c| c.is_ascii_digit() || c == 'S' || c == '-');
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

    /// `antspawn-session-<sid>-<nonce>`: one path component, no separators.
    pub fn pipe_name(&self, session_id: &str) -> String {
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
        let asked = match &req {
            Request::Exec { id, .. } => *id,
            Request::Close {} => unreachable!("returned above"),
        };
        let answer = match req.into_exec() {
            Ok((id, er)) => Response::from_outcome(id, &exec(&er)),
            Err(e) => error(Some(asked), e.to_string()),
        };
        if let Err(e) = reply(w, &answer) {
            return Ended::Broken(e);
        }
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
    let _ = std::fs::create_dir_all(scratch);
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
        let ended = serve_session(&mut requests(&[EXEC, r#"{"op":"close"}"#]), &mut out, |r| {
            calls += 1;
            assert_eq!(r.cmd, ["x"]);
            ok_outcome()
        });
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
    fn a_peer_that_goes_away_ends_the_session_as_disconnected() {
        // No close frame: the end of the stream is the launcher dying.
        let mut out = Vec::new();
        let ended = serve_session(&mut requests(&[EXEC]), &mut out, |_| ok_outcome());
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
        let ended = serve_session(&mut requests(&msgs), &mut out, |_| {
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
            let ended = serve_session(&mut Cursor::new(bytes), &mut out, |_| {
                panic!("{name}: reached the executor")
            });
            assert!(matches!(ended, Ended::Broken(_)), "{name}: {ended:?}");
        }
    }

    #[test]
    fn session_file_accepts_only_plain_values() {
        let ok = |sid: &str, nonce: &str| {
            SessionFile {
                launcher_sid: sid.into(),
                nonce: nonce.into(),
            }
            .validate()
        };
        let n = "0123456789abcdef";
        assert!(ok("S-1-5-21-1-2-3-1001", n).is_ok());
        for bad in ["", "S-1-5-21)(A;;GA;;;WD", "S-2-1", "S-1-5 21", "x"] {
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
