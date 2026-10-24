// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-jit-shim <runner> [args...]`: start the Actions runner with its JIT config in that one
//! child's environment, and nowhere else (CireSnave, board 260, "A: 1 B: 1").
//!
//! The JIT config arrives on **stdin**, which the host made an anonymous pipe; it is never an
//! argument, a file, or a variable of this process. It is set as `ACTIONS_RUNNER_INPUT_JITCONFIG` on
//! the one `Command` that starts `<runner>`, whose own stdin is null. Nothing here prints the value,
//! and no message contains it. Anything that is not a plausible JIT config (empty, oversized, not
//! base64) is refused before the runner starts.

use std::io::Read;
use std::process::{Command, ExitCode, Stdio};

const VAR: &str = "ACTIONS_RUNNER_INPUT_JITCONFIG";
/// A JIT config is about 4 KiB; anything far beyond that is not one.
const MAX_BYTES: u64 = 64 * 1024;

fn refuse(why: &str) -> ExitCode {
    eprintln!("spawn-jit-shim: {why}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(runner) = args.next() else {
        return refuse("usage: spawn-jit-shim <runner> [args...]");
    };
    let mut raw = Vec::new();
    if std::io::stdin()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut raw)
        .is_err()
    {
        return refuse("could not read the JIT config from stdin");
    }
    if raw.len() as u64 > MAX_BYTES {
        return refuse("the JIT config on stdin is too large");
    }
    let Ok(text) = String::from_utf8(raw) else {
        return refuse("the JIT config on stdin is not text");
    };
    let config = text.trim_end_matches(['\r', '\n']);
    let base64ish = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_');
    if config.is_empty() || !config.chars().all(base64ish) {
        return refuse("stdin does not hold a JIT config (empty or not base64)");
    }
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
