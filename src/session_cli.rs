// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn session`: one session, driven by lines (slice S5 of `docs/SESSION-PLAN.md`).
//!
//! A session lives in the process that opened it: when that process ends the host ends the session
//! (S4), which is the property that makes a killed owner safe. So the CLI is one long-lived
//! process, not three commands: it opens the session, reads one JSON request per line on its input,
//! writes one JSON reply per line on its output, and closes the session on `close` or end of input.
//!
//! The lines are the [`crate::session_proto`] messages, so a caller that already speaks the pipe
//! wire needs nothing new: `{"op":"exec","id":1,"cmd":["cmd","/c","dir"]}` answers with
//! `{"op":"exec","id":1,"status":"exited","exit_code":0,"stdout":"<base64>","stderr":"<base64>"}`.
//! A malformed or refused line is answered with an `error` and the session stays usable; a line
//! above [`crate::session_proto::MAX_FRAME`] ends it, because the stream can no longer be trusted.

use std::io::{self, BufRead, Read, Write};

use crate::session::Session;
use crate::session_proto::{MAX_FRAME, Request, Response};

fn reply<W: Write>(out: &mut W, r: &Response) -> io::Result<()> {
    let mut line = serde_json::to_vec(r).map_err(io::Error::other)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()
}

/// Serve `session` from `input` to `output` until `close` or end of input, then close it.
///
/// # Errors
/// An I/O error on `input` or `output`. The session is closed on every path out.
pub fn serve<R: BufRead, W: Write>(
    session: Box<dyn Session>,
    mut input: R,
    mut output: W,
) -> io::Result<()> {
    let mut session = Some(session);
    let result = serve_lines(&mut session, &mut input, &mut output);
    // end of input, a too-long line or an I/O error: the session still must not outlive us
    if let Some(s) = session.take()
        && let Err(e) = s.close()
    {
        let _ = reply(
            &mut output,
            &Response::Error {
                id: None,
                message: format!("close failed: {e}"),
            },
        );
    }
    result
}

fn serve_lines<R: BufRead, W: Write>(
    session: &mut Option<Box<dyn Session>>,
    input: &mut R,
    output: &mut W,
) -> io::Result<()> {
    loop {
        let mut line = Vec::new();
        let n = input
            .by_ref()
            .take(MAX_FRAME as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(());
        }
        if line.len() > MAX_FRAME {
            return reply(
                output,
                &Response::Error {
                    id: None,
                    message: "a line above the frame cap; the session is closed".into(),
                },
            );
        }
        let text = line.trim_ascii();
        if text.is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_slice(text) {
            Ok(r) => r,
            Err(e) => {
                reply(
                    output,
                    &Response::Error {
                        id: None,
                        message: format!("not a session request: {e}"),
                    },
                )?;
                continue;
            }
        };
        let id = match &request {
            Request::Exec { id, .. } => Some(*id),
            Request::Close {} => None,
        };
        if matches!(request, Request::Close {}) {
            let answer = match session.take() {
                Some(s) => match s.close() {
                    Ok(()) => Response::Closed {},
                    Err(e) => Response::Error {
                        id: None,
                        message: format!("close failed: {e}"),
                    },
                },
                None => Response::Closed {},
            };
            return reply(output, &answer);
        }
        let answer = match request.into_exec() {
            Err(e) => Response::Error {
                id,
                message: e.to_string(),
            },
            Ok((id, req)) => match session.as_mut().map(|s| s.exec(&req)) {
                Some(Ok(outcome)) => Response::from_outcome(id, &outcome),
                Some(Err(e)) => Response::Error {
                    id: Some(id),
                    message: e.to_string(),
                },
                None => Response::Error {
                    id: Some(id),
                    message: "the session is closed".into(),
                },
            },
        };
        reply(output, &answer)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::{Exit, LaunchError};
    use crate::session::{ExecOutcome, ExecRequest};
    use crate::streams::Streams;
    use std::sync::{Arc, Mutex};

    /// Records what it was asked and the close; echoes the first word back.
    struct Fake {
        log: Arc<Mutex<Vec<String>>>,
    }
    impl Session for Fake {
        fn exec(&mut self, req: &ExecRequest) -> Result<ExecOutcome, LaunchError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("exec {}", req.cmd[0]));
            if req.cmd[0] == "boom" {
                return Err(LaunchError::Other("host went away".into()));
            }
            Ok(ExecOutcome {
                exit: Exit::Code(0),
                streams: Streams {
                    stdout: Some(req.cmd[0].clone().into_bytes()),
                    stderr: Some(Vec::new()),
                    combined: false,
                },
            })
        }
        fn close(self: Box<Self>) -> Result<(), LaunchError> {
            self.log.lock().unwrap().push("close".into());
            Ok(())
        }
    }

    fn run(input: &str) -> (Vec<serde_json::Value>, Vec<String>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut out = Vec::new();
        serve(
            Box::new(Fake { log: log.clone() }),
            io::Cursor::new(input.as_bytes().to_vec()),
            &mut out,
        )
        .unwrap();
        let replies = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let log = log.lock().unwrap().clone();
        (replies, log)
    }

    #[test]
    fn exec_then_close_answers_in_order_and_closes_once() {
        let (r, log) = run(concat!(
            r#"{"op":"exec","id":7,"cmd":["hello"]}"#,
            "\n",
            r#"{"op":"exec","id":8,"cmd":["again"]}"#,
            "\n",
            r#"{"op":"close"}"#,
            "\n"
        ));
        assert_eq!(r.len(), 3);
        assert_eq!(r[0]["id"], 7);
        assert_eq!(r[0]["status"], "exited");
        assert_eq!(r[0]["exit_code"], 0);
        assert_eq!(r[0]["stdout"], "aGVsbG8=");
        assert_eq!(r[1]["id"], 8);
        assert_eq!(r[2]["op"], "closed");
        assert_eq!(log, ["exec hello", "exec again", "close"]);
    }

    #[test]
    fn end_of_input_closes_the_session_without_a_close_line() {
        let (r, log) = run("{\"op\":\"exec\",\"id\":1,\"cmd\":[\"x\"]}\n");
        assert_eq!(r.len(), 1);
        assert_eq!(
            log,
            ["exec x", "close"],
            "the owner went away, so the session is closed"
        );
    }

    #[test]
    fn a_bad_line_is_answered_and_the_session_stays_usable() {
        let (r, log) = run(concat!(
            "not json\n",
            r#"{"op":"exec","id":2,"cmd":[""]}"#,
            "\n",
            r#"{"op":"stop"}"#,
            "\n",
            r#"{"op":"close","x":1}"#,
            "\n\n",
            r#"{"op":"exec","id":3,"cmd":["ok"]}"#,
            "\n",
            r#"{"op":"close"}"#,
            "\n"
        ));
        let ops: Vec<_> = r.iter().map(|v| v["op"].as_str().unwrap()).collect();
        assert_eq!(ops, ["error", "error", "error", "error", "exec", "closed"]);
        assert_eq!(r[1]["id"], 2, "an invalid exec is refused under its own id");
        assert_eq!(
            log,
            ["exec ok", "close"],
            "refused lines never reached the session"
        );
    }

    #[test]
    fn a_failed_exec_is_an_error_under_its_id_and_the_session_goes_on() {
        let (r, log) = run(concat!(
            r#"{"op":"exec","id":4,"cmd":["boom"]}"#,
            "\n",
            r#"{"op":"exec","id":5,"cmd":["fine"]}"#,
            "\n",
            r#"{"op":"close"}"#,
            "\n"
        ));
        assert_eq!(r[0]["op"], "error");
        assert_eq!(r[0]["id"], 4);
        assert_eq!(r[1]["id"], 5);
        assert_eq!(log, ["exec boom", "exec fine", "close"]);
    }

    #[test]
    fn a_line_above_the_frame_cap_ends_the_session_and_closes_it() {
        let big = format!(
            "{{\"op\":\"exec\",\"id\":1,\"cmd\":[\"{}\"]}}\n",
            "a".repeat(MAX_FRAME)
        );
        let (r, log) = run(&big);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["op"], "error");
        assert_eq!(
            log,
            ["close"],
            "nothing ran, and the session did not survive the bad stream"
        );
    }
}
