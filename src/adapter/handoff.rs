// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host end of the runner hand-off: start `spawn-jit-shim` and give it the JIT config over an
//! anonymous pipe (CireSnave, board 260, "A: 1 B: 1"). The config is written to the shim's stdin
//! and nowhere else: not an argument, not a variable of this process, not a file.
//!
//! This is only the pipe. Starting the shim *inside* a launcher's environment (Windows Sandbox is
//! the only place the runner gets network) waits on the sandbox starting.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use super::github::Secret;

/// Start `shim` (which starts `runner` with `args`) and write `jit` to its stdin, then close it.
/// The returned child's stdout and stderr are inherited; its stdin is gone.
pub fn start_shim(
    shim: &Path,
    runner: &Path,
    args: &[OsString],
    jit: &Secret,
) -> std::io::Result<Child> {
    let mut child = Command::new(shim)
        .arg(runner)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    if let Err(e) = stdin.write_all(jit.expose().as_bytes()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    drop(stdin);
    Ok(child)
}
