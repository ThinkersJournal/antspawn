// SPDX-License-Identifier: MIT OR Apache-2.0
//! The wire format between a launcher and a session host (slice S2 of `docs/SESSION-PLAN.md`).
//!
//! Pure: no pipe, no process, no account. The host in S3 puts this on a named pipe whose DACL admits
//! the launcher's SID only; everything here is what must hold no matter who is on the other end.
//!
//! A frame is a 4-byte little-endian length, then that many bytes of JSON. The length is checked
//! BEFORE anything is allocated or read: zero and anything above [`MAX_FRAME`] are refused, so a peer
//! cannot make the host allocate or wait for a megabyte it announced. Messages deny unknown fields
//! and unknown `op` tags are errors, so a later `cancel` or `output` request (asks A4, A5) is refused
//! by a host that does not know it, never ignored.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::launcher::Exit;
use crate::session::{ExecOutcome, ExecRequest};
use crate::streams::{Streams, tail_of};

/// Largest payload a frame may carry, in bytes. Two 64 KiB stream tails as base64 fit well inside it.
pub const MAX_FRAME: usize = 1024 * 1024;

/// Why a frame could not be read, written or understood.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("frame length {0} is outside 1..={MAX_FRAME}")]
    BadLength(u64),
    #[error("the peer closed in the middle of a frame")]
    Truncated,
    #[error("frame is not a valid message: {0}")]
    Malformed(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// What a launcher may ask of a session host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Run one command. `id` is the caller's; the reply carries it back.
    Exec {
        id: u64,
        cmd: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time_s: Option<u64>,
    },
    /// End the session. The host answers [`Response::Closed`] and exits.
    /// (A struct variant, not a unit one: serde ignores unknown fields on unit variants of a
    /// tagged enum even with `deny_unknown_fields`; `{"op":"close","x":1}` must be an error.)
    Close {},
    /// Write `data` (base64, at most [`MAX_CHUNK`] bytes) into the session's workspace at `path`.
    /// `offset` 0 creates or truncates the file; any other offset must equal its current length, so a
    /// lost or repeated chunk is an error, never a silent gap. Slice W5 (copy-back): the launcher cannot
    /// read or write the account's work dir, so a project goes in, and changes come out, through here.
    Put {
        id: u64,
        path: String,
        offset: u64,
        data: String,
    },
    /// Read up to `len` (at most [`MAX_CHUNK`]) bytes of `path` from `offset`.
    Get {
        id: u64,
        path: String,
        offset: u64,
        len: u64,
    },
    /// List regular files in the workspace with size and SHA-256, sorted by path, starting after
    /// `after` (exclusive), at most [`MAX_LIST`] per answer.
    List {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
    },
}

/// Largest raw chunk of a file in one `put` or `get`; its base64 stays inside [`MAX_FRAME`].
pub const MAX_CHUNK: u64 = 512 * 1024;
/// Most entries in one `files` answer (about 150 bytes each as JSON, inside [`MAX_FRAME`]).
pub const MAX_LIST: usize = 2000;

/// One file in a `files` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// What a session host answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Exec {
        id: u64,
        status: Status,
        /// The command's own exit code, when it exited.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Last bytes of stdout and stderr, base64.
        stdout: String,
        stderr: String,
        /// Both streams are in `stdout` (the sandbox `2>&1` case); `stderr` is then empty.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        combined: bool,
    },
    Closed {},
    /// A `put` landed; `size` is the file's length now.
    Stored {
        id: u64,
        size: u64,
    },
    /// A `get` chunk (base64); `eof` when it reached the end of the file.
    Data {
        id: u64,
        data: String,
        eof: bool,
    },
    /// A `list` page; `more` when another page follows `files.last()`.
    Files {
        id: u64,
        files: Vec<FileInfo>,
        more: bool,
    },
    /// The request was refused or failed; the session is still usable unless the host then closes.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        message: String,
    },
}

/// How an exec ended (same words as `spawn-result.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Exited,
    TimedOut,
    Killed,
}

impl Request {
    /// The request for `req`, with the caller's `id`.
    pub fn exec(id: u64, req: &ExecRequest) -> Request {
        Request::Exec {
            id,
            cmd: req.cmd.clone(),
            cwd: req.cwd.clone(),
            env: req.env.clone(),
            time_s: req.time_s,
        }
    }

    /// The host side: the validated [`ExecRequest`] inside an `exec`. The host does not trust the
    /// launcher, so it runs the same checks the launcher does.
    pub fn into_exec(self) -> Result<(u64, ExecRequest), ProtoError> {
        match self {
            Request::Exec {
                id,
                cmd,
                cwd,
                env,
                time_s,
            } => {
                let req = ExecRequest {
                    cmd,
                    cwd,
                    env,
                    time_s,
                };
                req.validate().map_err(ProtoError::Invalid)?;
                Ok((id, req))
            }
            _ => Err(ProtoError::Invalid("not an exec".into())),
        }
    }
}

impl Response {
    /// The answer to exec `id` from how it ended.
    pub fn from_outcome(id: u64, outcome: &ExecOutcome) -> Response {
        let (status, exit_code) = match outcome.exit {
            Exit::Code(c) => (Status::Exited, Some(c)),
            Exit::TimedOut => (Status::TimedOut, None),
            Exit::Killed => (Status::Killed, None),
        };
        let enc = |b: &Option<Vec<u8>>| STANDARD.encode(tail_of(b.as_deref().unwrap_or_default()));
        Response::Exec {
            id,
            status,
            exit_code,
            stdout: enc(&outcome.streams.stdout),
            stderr: enc(&outcome.streams.stderr),
            combined: outcome.streams.combined,
        }
    }

    /// The launcher side: back to an [`ExecOutcome`]. An `exited` with no code is refused rather
    /// than guessed into a success.
    pub fn into_outcome(self) -> Result<(u64, ExecOutcome), ProtoError> {
        match self {
            Response::Exec {
                id,
                status,
                exit_code,
                stdout,
                stderr,
                combined,
            } => {
                let exit = match (status, exit_code) {
                    (Status::Exited, Some(c)) => Exit::Code(c),
                    (Status::Exited, None) => {
                        return Err(ProtoError::Invalid("`exited` without an exit_code".into()));
                    }
                    (Status::TimedOut, _) => Exit::TimedOut,
                    (Status::Killed, _) => Exit::Killed,
                };
                let dec = |s: &str| {
                    STANDARD
                        .decode(s)
                        .map_err(|e| ProtoError::Invalid(format!("stream is not base64: {e}")))
                };
                Ok((
                    id,
                    ExecOutcome {
                        exit,
                        streams: Streams {
                            stdout: Some(dec(&stdout)?),
                            stderr: Some(dec(&stderr)?),
                            combined,
                        },
                    },
                ))
            }
            Response::Error { message, .. } => Err(ProtoError::Invalid(message)),
            _ => Err(ProtoError::Invalid("not an exec result".into())),
        }
    }
}

/// Write one frame. A payload above [`MAX_FRAME`] is refused before a byte is written.
pub fn write_frame<W: Write>(w: &mut W, msg: &impl Serialize) -> Result<(), ProtoError> {
    let body = serde_json::to_vec(msg).map_err(|e| ProtoError::Malformed(e.to_string()))?;
    if body.is_empty() || body.len() > MAX_FRAME {
        return Err(ProtoError::BadLength(body.len() as u64));
    }
    let len = u32::try_from(body.len()).expect("MAX_FRAME fits u32");
    // One write, so a frame is never split across two small writes by this side.
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&body);
    w.write_all(&frame)?;
    w.flush()?;
    Ok(())
}

/// Read one frame. `Ok(None)` is a clean end of stream between frames; an end inside a frame is
/// [`ProtoError::Truncated`].
pub fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(
    r: &mut R,
) -> Result<Option<T>, ProtoError> {
    let mut head = [0u8; 4];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..])? {
            0 if got == 0 => return Ok(None),
            0 => return Err(ProtoError::Truncated),
            n => got += n,
        }
    }
    let len = u64::from(u32::from_le_bytes(head));
    if len == 0 || len > MAX_FRAME as u64 {
        return Err(ProtoError::BadLength(len));
    }
    let mut body = vec![0u8; usize::try_from(len).expect("<= MAX_FRAME")];
    r.read_exact(&mut body).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ProtoError::Truncated
        } else {
            ProtoError::Io(e)
        }
    })?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| ProtoError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frame_of(body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    fn read_req(bytes: Vec<u8>) -> Result<Option<Request>, ProtoError> {
        read_frame(&mut Cursor::new(bytes))
    }

    #[test]
    fn requests_and_responses_round_trip() {
        let mut er = ExecRequest::new(["cmd", "arg"]);
        er.cwd = Some("src".into());
        er.env.insert("K".into(), "V".into());
        er.time_s = Some(5);
        let req = Request::exec(7, &er);
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        write_frame(&mut buf, &Request::Close {}).unwrap();
        let mut c = Cursor::new(buf);
        assert_eq!(read_frame::<_, Request>(&mut c).unwrap(), Some(req.clone()));
        assert_eq!(
            read_frame::<_, Request>(&mut c).unwrap(),
            Some(Request::Close {})
        );
        assert!(
            read_frame::<_, Request>(&mut c).unwrap().is_none(),
            "clean end"
        );
        assert_eq!(req.into_exec().unwrap(), (7, er));
    }

    #[test]
    fn an_outcome_round_trips_including_binary_streams() {
        let out = ExecOutcome {
            exit: Exit::Code(3),
            streams: Streams {
                stdout: Some(vec![0, 255, 10, 13]),
                stderr: Some(b"err".to_vec()),
                combined: false,
            },
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &Response::from_outcome(9, &out)).unwrap();
        let back: Response = read_frame(&mut Cursor::new(buf)).unwrap().unwrap();
        assert_eq!(back.into_outcome().unwrap(), (9, out));
        for e in [Exit::TimedOut, Exit::Killed] {
            let o = ExecOutcome {
                exit: e,
                streams: Streams::default(),
            };
            let (_, back) = Response::from_outcome(1, &o).into_outcome().unwrap();
            assert_eq!(back.exit, e);
        }
    }

    #[test]
    fn exited_without_a_code_is_refused_not_read_as_success() {
        let r = Response::Exec {
            id: 1,
            status: Status::Exited,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            combined: false,
        };
        assert!(r.into_outcome().is_err());
    }

    #[test]
    fn a_combined_outcome_round_trips_and_a_frame_without_the_field_means_not_combined() {
        // Was dropped to `false` on the way through (pip review of #72): a consumer would misread which stream holds data.
        let out = ExecOutcome {
            exit: Exit::Code(0),
            streams: Streams {
                stdout: Some(b"both".to_vec()),
                stderr: None,
                combined: true,
            },
        };
        let wire = serde_json::to_string(&Response::from_outcome(4, &out)).unwrap();
        assert!(wire.contains("\"combined\":true"), "{wire}");
        let back: Response = serde_json::from_str(&wire).unwrap();
        let (_, got) = back.into_outcome().unwrap();
        assert!(got.streams.combined);
        // Control: a frame an older host wrote (no `combined`) is read as not combined, and is not refused.
        let old = r#"{"op":"exec","id":5,"status":"exited","exit_code":0,"stdout":"","stderr":""}"#;
        let (_, got) = serde_json::from_str::<Response>(old)
            .unwrap()
            .into_outcome()
            .unwrap();
        assert!(!got.streams.combined);
    }

    #[test]
    fn a_huge_announced_length_is_refused_before_any_body_is_read() {
        // The reader panics if asked for more than the 4 header bytes: proof nothing was read or allocated.
        struct HeaderOnly(Cursor<Vec<u8>>);
        impl Read for HeaderOnly {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                assert!(self.0.position() < 4, "read past the header");
                let n = buf.len().min(4 - self.0.position() as usize);
                self.0.read(&mut buf[..n])
            }
        }
        for len in [MAX_FRAME as u32 + 1, u32::MAX] {
            let mut r = HeaderOnly(Cursor::new(len.to_le_bytes().to_vec()));
            let e = read_frame::<_, Request>(&mut r).unwrap_err();
            assert!(
                matches!(e, ProtoError::BadLength(l) if l == u64::from(len)),
                "{e}"
            );
        }
    }

    #[test]
    fn zero_length_and_truncated_frames_are_refused() {
        assert!(matches!(
            read_req(0u32.to_le_bytes().to_vec()),
            Err(ProtoError::BadLength(0))
        ));
        // Half a header, a header with no body, and a body cut short.
        assert!(matches!(read_req(vec![1, 0]), Err(ProtoError::Truncated)));
        assert!(matches!(
            read_req(10u32.to_le_bytes().to_vec()),
            Err(ProtoError::Truncated)
        ));
        let mut cut = frame_of(br#"{"op":"close"}"#);
        cut.pop();
        assert!(matches!(read_req(cut), Err(ProtoError::Truncated)));
        // Control: the same frame, whole, reads.
        assert_eq!(
            read_req(frame_of(br#"{"op":"close"}"#)).unwrap(),
            Some(Request::Close {})
        );
    }

    #[test]
    fn bad_json_unknown_ops_and_unknown_fields_are_errors() {
        for body in [
            &b"not json"[..],
            br#"{"op":"cancel","id":1}"#,
            br#"{"op":"output"}"#,
            br#"{"op":"close","extra":1}"#,
            br#"{"op":"exec","id":1,"cmd":["x"],"shell":true}"#,
            br#"{"cmd":["x"]}"#,
        ] {
            assert!(
                matches!(read_req(frame_of(body)), Err(ProtoError::Malformed(_))),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn an_oversized_payload_is_refused_on_write_and_nothing_is_written() {
        let big = Request::Exec {
            id: 1,
            cmd: vec!["x".repeat(MAX_FRAME)],
            cwd: None,
            env: BTreeMap::new(),
            time_s: None,
        };
        let mut buf = Vec::new();
        assert!(matches!(
            write_frame(&mut buf, &big),
            Err(ProtoError::BadLength(_))
        ));
        assert!(buf.is_empty());
    }

    #[test]
    fn file_requests_and_answers_round_trip_and_deny_unknown_fields() {
        let reqs = [
            Request::Put {
                id: 1,
                path: "a/b".into(),
                offset: 0,
                data: "aGk=".into(),
            },
            Request::Get {
                id: 2,
                path: "a/b".into(),
                offset: 1,
                len: 9,
            },
            Request::List {
                id: 3,
                after: Some("a".into()),
            },
            Request::List { id: 4, after: None },
        ];
        for r in reqs {
            let mut buf = Vec::new();
            write_frame(&mut buf, &r).unwrap();
            assert_eq!(
                read_frame::<_, Request>(&mut Cursor::new(buf)).unwrap(),
                Some(r)
            );
        }
        let resp = Response::Files {
            id: 3,
            files: vec![FileInfo {
                path: "x".into(),
                size: 1,
                sha256: "00".into(),
            }],
            more: true,
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &resp).unwrap();
        assert_eq!(
            read_frame::<_, Response>(&mut Cursor::new(buf)).unwrap(),
            Some(resp)
        );
        for bad in [
            r#"{"op":"put","id":1,"path":"a","offset":0,"data":"","x":1}"#,
            r#"{"op":"list","id":1,"extra":true}"#,
            r#"{"op":"get","id":1,"path":"a","offset":0}"#,
        ] {
            assert!(read_req(frame_of(bad.as_bytes())).is_err(), "{bad}");
        }
        // a chunk at the limit still fits a frame once base64'd and wrapped
        let max = Request::Put {
            id: u64::MAX,
            path: "p".repeat(255),
            offset: u64::MAX,
            data: STANDARD.encode(vec![0u8; MAX_CHUNK as usize]),
        };
        write_frame(&mut Vec::new(), &max).expect("a maximal chunk fits a frame");
    }

    #[test]
    fn the_host_revalidates_what_the_launcher_sent() {
        for bad in [
            r#"{"op":"exec","id":1,"cmd":[]}"#,
            r#"{"op":"exec","id":1,"cmd":["x"],"cwd":".."}"#,
            r#"{"op":"exec","id":1,"cmd":["x"],"cwd":"C:\\x"}"#,
            r#"{"op":"exec","id":1,"cmd":["x"],"time_s":0}"#,
            r#"{"op":"exec","id":1,"cmd":["x"],"env":{"A=B":"v"}}"#,
        ] {
            let req = read_req(frame_of(bad.as_bytes())).unwrap().unwrap();
            assert!(
                matches!(req.into_exec(), Err(ProtoError::Invalid(_))),
                "{bad}"
            );
        }
    }
}
