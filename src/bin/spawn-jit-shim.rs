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
//!
//! With `--fresh <src> <under>` (Windows only; design item 4) the runner does not run in a shared
//! directory: after the config arrives, `<src>` (a template the account cannot write) is copied to a new
//! `<under>\run-<pid>-<nanos>`, `<runner>` is a path RELATIVE to that copy, the shim joins a kill-on-close
//! Job Object so everything the runner starts is in it, and when the runner exits every other process in
//! the job is terminated and the copy is deleted. The runner's TEMP, TMP, USERPROFILE, APPDATA and
//! LOCALAPPDATA point into `<copy>\_home`, so its scratch space goes with the copy. A copy that cannot be deleted is exit 3, never silent.

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

mod fresh {
    use std::ffi::{OsStr, OsString};
    use std::path::{Component, Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    /// A runner path inside the copy: relative, no `..`, no drive or root.
    pub fn relative_ok(p: &OsStr) -> bool {
        let path = Path::new(p);
        path.components().next().is_some()
            && path.components().all(|c| matches!(c, Component::Normal(_)))
    }

    /// Copy `src` into the existing directory `dst`. A symlink or junction in the template is refused.
    fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let to = dst.join(entry.file_name());
            if kind.is_symlink() {
                return Err(std::io::Error::other("the template holds a symbolic link"));
            }
            if kind.is_dir() {
                std::fs::create_dir(&to)?;
                copy_tree(&entry.path(), &to)?;
            } else {
                std::fs::copy(entry.path(), &to)?;
            }
        }
        Ok(())
    }

    fn remove_with_retries(dir: &Path) -> std::io::Result<()> {
        let mut last = Ok(());
        for _ in 0..25 {
            last = std::fs::remove_dir_all(dir);
            if last.is_ok() || !dir.exists() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        last
    }

    /// Run `runner` (relative to a fresh copy of `src` under `under`) with `config` in its environment.
    /// `Ok` is the runner's exit code; a copy left behind is an `Err`.
    pub fn run(
        src: &Path,
        under: &Path,
        runner: &OsStr,
        args: Vec<OsString>,
        var: &str,
        config: String,
    ) -> Result<i32, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir: PathBuf = under.join(format!("run-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir)
            .map_err(|e| format!("could not create the run directory: {}", e.kind()))?;
        let result = start(src, &dir, runner, args, var, config);
        match (result, remove_with_retries(&dir)) {
            (Ok(code), Ok(())) => Ok(code),
            (Err(e), Ok(())) => Err(e),
            (_, Err(e)) => Err(format!("could not delete {}: {}", dir.display(), e.kind())),
        }
    }

    fn start(
        src: &Path,
        dir: &Path,
        runner: &OsStr,
        args: Vec<OsString>,
        var: &str,
        config: String,
    ) -> Result<i32, String> {
        copy_tree(src, dir).map_err(|e| format!("could not copy the template: {}", e.kind()))?;
        // The runner's temp and profile folders live inside the copy, so they are deleted with it. This
        // reduces what a job leaves for the next one; it cannot stop a job writing the real profile
        // by absolute path.
        let home = dir.join("_home");
        let (temp, roaming, local) = (
            home.join("Temp"),
            home.join("AppData").join("Roaming"),
            home.join("AppData").join("Local"),
        );
        for d in [&temp, &roaming, &local] {
            std::fs::create_dir_all(d)
                .map_err(|e| format!("could not create the temp folders: {}", e.kind()))?;
        }
        super::contain::join()?;
        let status = Command::new(dir.join(runner))
            .args(args)
            .current_dir(dir)
            .env("TEMP", &temp)
            .env("TMP", &temp)
            .env("USERPROFILE", &home)
            .env("APPDATA", &roaming)
            .env("LOCALAPPDATA", &local)
            .env(var, config)
            .stdin(Stdio::null())
            .status()
            .map_err(|e| format!("could not start the runner: {}", e.kind()))?;
        super::contain::sweep()?;
        Ok(status.code().unwrap_or(1))
    }
}

#[cfg(windows)]
mod contain {
    //! The Win32 Job Object calls (the shim's only other `unsafe`, with the pipe).
    use std::mem::size_of;
    use std::sync::atomic::{AtomicPtr, Ordering};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_TERMINATE, TerminateProcess,
    };

    static JOB: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

    /// Create a kill-on-close job and put this process in it; children inherit it.
    pub fn join() -> Result<(), String> {
        // SAFETY: plain Win32 calls on valid pointers to locals; the handle is checked and kept for the
        // life of the process (closing it, at exit, is what kills any straggler).
        unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return Err(format!(
                    "CreateJobObjectW: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
                || AssignProcessToJobObject(h, GetCurrentProcess()) == 0
            {
                let e = std::io::Error::last_os_error();
                CloseHandle(h);
                return Err(format!("could not contain the runner: {e}"));
            }
            JOB.store(h, Ordering::SeqCst);
        }
        Ok(())
    }

    /// PIDs in the job, from `JobObjectBasicProcessIdList` (room for 1023).
    fn members(job: HANDLE) -> Result<Vec<u32>, String> {
        let mut buf = vec![0u64; 1024];
        // SAFETY: `buf` is a writable block of the stated size; the call fills the documented layout
        // (two u32 counts, then pointer-sized ids).
        let ok = unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicProcessIdList,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(format!(
                "QueryInformationJobObject: {}",
                std::io::Error::last_os_error()
            ));
        }
        let listed = ((buf[0] >> 32) as usize).min(1023);
        Ok(buf[1..=listed].iter().map(|&p| p as u32).collect())
    }

    /// Terminate every process in the job except this one, and wait until only this one is left.
    pub fn sweep() -> Result<(), String> {
        let job: HANDLE = JOB.load(Ordering::SeqCst);
        let me = std::process::id();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let others: Vec<u32> = members(job)?.into_iter().filter(|&p| p != me).collect();
            if others.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{} process(es) outlived the runner and could not be ended",
                    others.len()
                ));
            }
            for pid in others {
                // SAFETY: a pid from the job list; the handle is closed at once.
                unsafe {
                    let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
                    if !h.is_null() {
                        TerminateProcess(h, 1);
                        CloseHandle(h);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(not(windows))]
mod contain {
    pub fn join() -> Result<(), String> {
        Err("--fresh is only available on Windows".into())
    }
    pub fn sweep() -> Result<(), String> {
        Ok(())
    }
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
    let fresh = if args.peek().is_some_and(|a| a == "--fresh") {
        args.next();
        match (args.next(), args.next()) {
            (Some(src), Some(under))
                if std::path::Path::new(&src).is_dir() && std::path::Path::new(&under).is_dir() =>
            {
                Some((
                    std::path::PathBuf::from(src),
                    std::path::PathBuf::from(under),
                ))
            }
            _ => {
                return refuse(
                    "--fresh needs <template dir> <runs dir>, both existing directories",
                );
            }
        }
    } else {
        None
    };
    let Some(runner) = args.next() else {
        return refuse(
            "usage: spawn-jit-shim [--listen <addr:port> | --pipe <name> --allow-sid <sid>] [--fresh <template> <runs>] <runner> [args...]",
        );
    };
    if fresh.is_some() && !fresh::relative_ok(&runner) {
        return refuse("with --fresh the runner is a path inside the copy: relative, no '..'");
    }
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
    if let Some((src, under)) = fresh {
        return match fresh::run(&src, &under, &runner, args.collect(), VAR, config) {
            Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
            Err(why) => {
                eprintln!("spawn-jit-shim: {why}");
                ExitCode::from(3)
            }
        };
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
