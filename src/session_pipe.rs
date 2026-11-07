// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Win32 half of a session host that is not the Job Object (slice S3 of `docs/SESSION-PLAN.md`): the
//! session's named pipe with its one-principal DACL, and the owner of a file. It is a library module, not
//! part of `spawn-runner`, so the integration tests exercise the real pipe and DACL in-process.
//! The only `unsafe` besides the runner's Job Object calls.

#[cfg(windows)]
mod imp {
    use std::mem::size_of;
    use std::path::Path;

    /// Create the session's one-instance duplex pipe, DACL admitting `allow_sid` only, remote clients
    /// rejected, and wait (bounded) for its one client. The host never needs an ACE of its own.
    pub fn accept(name: &str, allow_sid: &str) -> Result<std::fs::File, String> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use std::sync::mpsc;
        use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, TRUE};
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
        };
        use windows_sys::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_WAIT,
        };

        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        // Protected DACL, one ACE: 0x12019f is FILE_GENERIC_READ | FILE_GENERIC_WRITE spelled out as
        // specific rights (the generic bits are not mapped in a pipe DACL; the JIT shim's pipe measured
        // that). No inheritance, no other principal.
        let sddl = wide(&format!("D:P(A;;0x12019f;;;{allow_sid})"));
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; `descriptor` receives a LocalAlloc'd block kept for the
        // life of this process (the pipe is created from it exactly once).
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
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
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                64 * 1024,
                0,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(format!(
                "could not create the pipe (it may already exist): {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `handle` is a valid pipe handle owned by nothing else; `File` now owns it.
        let pipe = unsafe { std::fs::File::from_raw_handle(handle as _) };
        let raw = pipe.as_raw_handle() as usize;
        // ConnectNamedPipe blocks, so the wait is a thread; if it never returns the process exits.
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            // SAFETY: the handle lives until this function returns or the process exits.
            let connected = unsafe { ConnectNamedPipe(raw as _, std::ptr::null_mut()) };
            // ERROR_PIPE_CONNECTED (535): the client got in before this call; still connected.
            let up = connected == TRUE
                || matches!(std::io::Error::last_os_error().raw_os_error(), Some(535));
            let _ = done.send(up);
        });
        match result.recv_timeout(std::time::Duration::from_secs(60)) {
            Ok(true) => Ok(pipe),
            Ok(false) => Err("accept failed".into()),
            Err(_) => {
                // The wait thread is still blocked in ConnectNamedPipe on this handle. Closing it now would
                // pull the handle out from under that thread, so it is leaked on purpose: the caller exits the
                // process right after an error. A caller that retried instead would have to keep this in mind.
                std::mem::forget(pipe);
                Err("no client connected in time".into())
            }
        }
    }

    /// The owner of `path` as a SID string (`S-1-5-21-...`), as the OS reports it.
    pub fn owner_sid(path: &Path) -> Result<String, String> {
        use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;

        let wide: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut owner = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; `owner` points into `descriptor`, which is LocalAlloc'd and
        // freed below after the SID has been copied out.
        let rc = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(format!(
                "could not read the owner of {}: error {rc}",
                path.display()
            ));
        }
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: `owner` is a valid SID inside `descriptor`.
        let ok = unsafe { ConvertSidToStringSidW(owner, &mut text) };
        let result = if ok == 0 || text.is_null() {
            Err(format!(
                "could not format the owner SID of {}",
                path.display()
            ))
        } else {
            // SAFETY: `text` is a NUL-terminated wide string from ConvertSidToStringSidW.
            let s = unsafe {
                let mut len = 0;
                while *text.add(len) != 0 {
                    len += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
            };
            Ok(s)
        };
        // SAFETY: both blocks were allocated by the calls above and are not used again.
        unsafe {
            if !text.is_null() {
                LocalFree(text as _);
            }
            LocalFree(descriptor as _);
        }
        result
    }
}

#[cfg(windows)]
pub use imp::{accept, owner_sid};

/// The runner is Windows-only; elsewhere these refuse rather than pretend.
#[cfg(not(windows))]
pub fn accept(_: &str, _: &str) -> Result<std::fs::File, String> {
    Err("session pipes are implemented for Windows only".into())
}
#[cfg(not(windows))]
pub fn owner_sid(_: &std::path::Path) -> Result<String, String> {
    Err("file owners are read on Windows only".into())
}
