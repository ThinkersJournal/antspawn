// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-jit-shim [--listen <addr:port> | --pipe <name> --allow-sid <sid>] <runner> [args...]`: start the Actions runner with its JIT
//! config in that one child's environment, and nowhere else (CireSnave, board 260, "A: 1 B: 1").
//!
//! The JIT config arrives either on **stdin** (an anonymous pipe the host made) or, with `--listen`,
//! on **one TCP connection** (board 322, ruled A: Windows Sandbox's `wsb exec` forwards no stdin). In
//! listen mode the shim binds, accepts EXACTLY ONE connection, and closes the listener before reading
//! a byte, so a second connection, or one after the first, is refused. It is never an argument, a
//! file, or a variable of this process; it is set as `ACTIONS_RUNNER_INPUT_JITCONFIG` on the one
//! `Command` that starts `<runner>`, whose own stdin is null. Nothing here prints the value, and no
//! message contains it.
//!
//! With `--pipe` (Windows only; ROADMAP row 21, board 341) the config arrives on a **named pipe** that
//! exists for exactly one client: one instance, remote clients rejected, and a protected DACL that grants
//! write to `--allow-sid` and to nobody else, the shim's own account included. The shim is started by
//! Task Scheduler as the runner account, so the host cannot hand it an anonymous pipe; this is the channel
//! that crosses two accounts without a file. Anything that is not a plausible JIT config (empty, oversized, not base64) is
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

/// A pipe name is one path component: no separators, so `\.\pipe\<name>` cannot be redirected.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A SID in string form, `S-1-5-21-...`; checked here so it cannot carry SDDL syntax.
fn valid_sid(sid: &str) -> bool {
    sid.starts_with("S-1-")
        && sid.len() <= 184
        && sid
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'S' | '-'))
}

#[cfg(windows)]
mod pipe {
    use std::fs::File;
    use std::os::windows::io::FromRawHandle;
    use std::ptr::null_mut;
    use std::sync::mpsc;

    use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, TRUE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    use super::{ACCEPT_WINDOW, MAX_BYTES, READ_WINDOW, read_config};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Create the one-instance pipe, wait for its one client, read the config and close the pipe.
    pub fn receive(name: &str, allow_sid: &str) -> Result<Result<String, &'static str>, String> {
        // Protected DACL (no inheritance), one ACE for `allow_sid`: 0x120196 is SDDL "FW"
        // (FILE_GENERIC_WRITE) plus FILE_READ_ATTRIBUTES, which a client's CreateFile asks for and
        // "FW" alone refuses (measured here: "FW" gave access denied to the named SID; generic bits
        // are not mapped in a pipe DACL either). No read right, and not even the owner is named; the
        // server end is opened by this process and needs no ACE.
        let sddl = wide(&format!("D:P(A;;0x120196;;;{allow_sid})"));
        let mut descriptor = null_mut();
        // SAFETY: `sddl` is NUL-terminated; `descriptor` receives a LocalAlloc'd block that is kept
        // for the life of this short-lived process (the pipe is created from it exactly once).
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err("could not build the pipe's access list".into());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let path = wide(&format!(r"\\.\pipe\{name}"));
        // SAFETY: `path` is NUL-terminated and `attributes` outlives the call.
        let handle = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                MAX_BYTES as u32 + 1,
                0,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err("could not create the pipe (it may already exist)".into());
        }
        // SAFETY: `handle` is a valid pipe handle owned by nothing else; `File` now owns it.
        let pipe = unsafe { File::from_raw_handle(handle as _) };
        println!("spawn-jit-shim: pipe ready");
        let raw = pipe_raw(&pipe);
        // ConnectNamedPipe blocks, so the wait is a thread; the process exits if it never returns.
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            // SAFETY: the handle lives until the thread's result is taken or the process exits.
            let connected = unsafe { ConnectNamedPipe(raw as _, null_mut()) };
            let _ = done.send(connected == TRUE || last_error_is_connected());
        });
        match result.recv_timeout(ACCEPT_WINDOW) {
            Ok(true) => {}
            Ok(false) => return Err("accept failed".into()),
            Err(_) => return Err("no connection arrived in time".into()),
        }
        // The one instance is now taken: any further client finds the pipe busy. Bound the read.
        let (read_done, read_result) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = read_done.send(read_config(pipe));
        });
        read_result
            .recv_timeout(READ_WINDOW)
            .map(Ok)
            .map_err(|_| "the client did not finish sending in time".to_owned())
            .and_then(|r| r)
    }

    fn pipe_raw(f: &File) -> usize {
        use std::os::windows::io::AsRawHandle;
        f.as_raw_handle() as usize
    }

    /// `ConnectNamedPipe` returns 0 when the client got in first: ERROR_PIPE_CONNECTED (535), or
    /// ERROR_NO_DATA (232) when it has already written and closed. Its bytes are still readable.
    fn last_error_is_connected() -> bool {
        matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(535 | 232)
        )
    }
}

#[cfg(windows)]
fn receive_pipe(name: &str, sid: &str) -> Result<Result<String, &'static str>, String> {
    pipe::receive(name, sid)
}

#[cfg(not(windows))]
fn receive_pipe(_: &str, _: &str) -> Result<Result<String, &'static str>, String> {
    Err("--pipe is only available on Windows".into())
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
    let pipe = if args.peek().is_some_and(|a| a == "--pipe") {
        args.next();
        let name = args.next().and_then(|a| a.into_string().ok());
        let sid = args
            .next()
            .filter(|a| a == "--allow-sid")
            .and_then(|_| args.next())
            .and_then(|a| a.into_string().ok());
        match (name, sid) {
            (Some(name), Some(sid)) if valid_name(&name) && valid_sid(&sid) => Some((name, sid)),
            _ => return refuse("--pipe needs <name> --allow-sid <S-1-...>"),
        }
    } else {
        None
    };
    let Some(runner) = args.next() else {
        return refuse(
            "usage: spawn-jit-shim [--listen <addr:port> | --pipe <name> --allow-sid <sid>] <runner> [args...]",
        );
    };
    let read = if let Some((name, sid)) = pipe {
        match receive_pipe(&name, &sid) {
            Ok(read) => read,
            Err(why) => return refuse(&why),
        }
    } else {
        match listen {
            Some(addr) => match accept_one(addr) {
                Ok(stream) => read_config(stream),
                Err(why) => return refuse(&why),
            },
            None => read_config(std::io::stdin()),
        }
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
