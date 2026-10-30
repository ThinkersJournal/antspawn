// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-jit-shim [--listen <addr:port>] <runner> [args...]`: start the Actions runner with its JIT
//! config in that one child's environment, and nowhere else (CireSnave, board 260, "A: 1 B: 1").
//!
//! The JIT config arrives either on **stdin** (an anonymous pipe the host made) or, with `--listen`,
//! on **one TCP connection** (board 322, ruled A: Windows Sandbox's `wsb exec` forwards no stdin). In
//! listen mode the shim binds, accepts EXACTLY ONE connection, and closes the listener before reading
//! a byte, so a second connection, or one after the first, is refused. It is never an argument, a
//! file, or a variable of this process; it is set as `ACTIONS_RUNNER_INPUT_JITCONFIG` on the one
//! `Command` that starts `<runner>`, whose own stdin is null. Nothing here prints the value, and no
//! message contains it. Anything that is not a plausible JIT config (empty, oversized, not base64) is
//! refused before the runner starts.

use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const VAR: &str = "ACTIONS_RUNNER_INPUT_JITCONFIG";
/// A JIT config is about 4 KiB; anything far beyond that is not one.
const MAX_BYTES: u64 = 64 * 1024;
/// How long the listener waits for the host's one connection, and how long the host may take to send.
const ACCEPT_WINDOW: Duration = Duration::from_secs(300);
const READ_WINDOW: Duration = Duration::from_secs(30);

fn refuse(why: &str) -> ExitCode {
    eprintln!("spawn-jit-shim: {why}");
    ExitCode::from(2)
}

/// Bind `addr`, announce where, take exactly one connection and close the listener. The returned
/// stream is the only way in; there is nothing left to connect to.
fn accept_one(addr: SocketAddr) -> Result<std::net::TcpStream, String> {
    let listener =
        TcpListener::bind(addr).map_err(|e| format!("could not listen: {}", e.kind()))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("could not listen: {}", e.kind()))?;
    let bound = listener.local_addr().map_err(|e| e.kind().to_string())?;
    println!("spawn-jit-shim: listening on {bound}");
    let deadline = Instant::now() + ACCEPT_WINDOW;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                drop(listener);
                stream
                    .set_nonblocking(false)
                    .and_then(|()| stream.set_read_timeout(Some(READ_WINDOW)))
                    .map_err(|e| format!("could not read: {}", e.kind()))?;
                return Ok(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("no connection arrived in time".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("accept failed: {}", e.kind())),
        }
    }
}

fn read_config(mut source: impl Read) -> Result<String, &'static str> {
    let mut raw = Vec::new();
    if (&mut source)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut raw)
        .is_err()
    {
        return Err("could not read the JIT config");
    }
    if raw.len() as u64 > MAX_BYTES {
        return Err("the JIT config is too large");
    }
    let Ok(text) = String::from_utf8(raw) else {
        return Err("the JIT config is not text");
    };
    let config = text.trim_end_matches(['\r', '\n']);
    let base64ish = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_');
    if config.is_empty() || !config.chars().all(base64ish) {
        return Err("the input does not hold a JIT config (empty or not base64)");
    }
    Ok(config.to_owned())
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1).peekable();
    let listen = if args.peek().is_some_and(|a| a == "--listen") {
        args.next();
        let parsed = args
            .next()
            .and_then(|a| a.into_string().ok())
            .and_then(|a| a.parse::<SocketAddr>().ok());
        match parsed {
            Some(addr) => Some(addr),
            None => return refuse("--listen needs an address like 0.0.0.0:7788"),
        }
    } else {
        None
    };
    let Some(runner) = args.next() else {
        return refuse("usage: spawn-jit-shim [--listen <addr:port>] <runner> [args...]");
    };
    let read = match listen {
        Some(addr) => match accept_one(addr) {
            Ok(stream) => read_config(stream),
            Err(why) => return refuse(&why),
        },
        None => read_config(std::io::stdin()),
    };
    let config = match read {
        Ok(config) => config,
        Err(why) => return refuse(why),
    };
    match Command::new(runner)
        .args(args)
        .env(VAR, config)
        .stdin(Stdio::null())
        .status()
    {
        Ok(status) => ExitCode::from(status.code().map_or(1, |c| u8::try_from(c).unwrap_or(1))),
        Err(e) => refuse(&format!("could not start the runner: {}", e.kind())),
    }
}
