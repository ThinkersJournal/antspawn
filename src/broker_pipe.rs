// SPDX-License-Identifier: MIT OR Apache-2.0
//! The broker's pipe (slice V3 of `docs/SANDBOX-BROKER.md`): many clients over time, each one's
//! SID read from the OS, never from anything the client sends.
//!
//! [`Listener::accept`] returns the connected pipe and the SID of the account that connected, read
//! from the client's process token (see `client_sid` for the one residual race). The pipe's DACL admits
//! only the SIDs the broker was configured with, remote clients are rejected, and the name is the
//! first instance of its kind (a second broker, or something squatting the name, makes `new` fail).
//!
//! The next instance is created before the connected one is handed out, so there is no window with
//! no listening instance. Shown by the tests: the SID read back for the calling account, a refused
//! client, a second concurrent client, a name that is already taken. NOT shown: a client from a
//! different account (the DACL path needs a second account; the accounts are CireSnave's).
//! The only `unsafe` besides the runner's Job Object calls and `session_pipe`.

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use crate::session_host::valid_sid;

    pub struct Listener {
        path: Vec<u16>,
        sddl: Vec<u16>,
        pending: File,
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn instance(path: &[u16], sddl: &[u16], first: bool) -> Result<File, String> {
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; `descriptor` receives a LocalAlloc'd block freed below.
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
        let mode = PIPE_ACCESS_DUPLEX
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        // SAFETY: `path` is NUL-terminated and `attributes` outlives the call.
        let handle = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                &attributes,
            )
        };
        let failure = std::io::Error::last_os_error();
        // SAFETY: allocated by the conversion above and not used again.
        unsafe { LocalFree(descriptor as _) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(format!("could not create the pipe: {failure}"));
        }
        // SAFETY: a valid pipe handle owned by nothing else; `File` now owns it.
        Ok(unsafe { File::from_raw_handle(handle as _) })
    }

    impl Listener {
        /// Create the pipe `name` (a plain component) admitting exactly `allow` (SIDs).
        ///
        /// # Errors
        /// A bad name or SID, no SID at all, or the pipe could not be created as the first of its name.
        pub fn new(name: &str, allow: &[String]) -> Result<Self, String> {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err("pipe name must be 1..=64 of a-z A-Z 0-9 - _".into());
            }
            if allow.is_empty() || allow.len() > 16 || !allow.iter().all(|s| valid_sid(s)) {
                return Err("the pipe needs 1..=16 well-formed SIDs".into());
            }
            // Protected DACL, one ACE per SID, 0x12019f = read and write spelled as specific rights.
            let aces: String = allow
                .iter()
                .map(|s| format!("(A;;0x12019f;;;{s})"))
                .collect();
            let sddl = wide(&format!("D:P{aces}"));
            let path = wide(&format!(r"\\.\pipe\{name}"));
            let pending = instance(&path, &sddl, true)?;
            Ok(Self {
                path,
                sddl,
                pending,
            })
        }

        /// Wait for one client. Returns the connected pipe and the client's SID as the OS reports it.
        /// The next instance exists before this returns.
        ///
        /// # Errors
        /// The connect failed, the client's token could not be read (the client is dropped), or the
        /// next instance could not be created.
        pub fn accept(&mut self) -> Result<(File, String), String> {
            let next = instance(&self.path, &self.sddl, false)?;
            let connected = std::mem::replace(&mut self.pending, next);
            let raw = connected.as_raw_handle() as HANDLE;
            // SAFETY: `raw` is the valid pipe handle `connected` owns.
            let ok = unsafe { ConnectNamedPipe(raw, std::ptr::null_mut()) };
            if ok == 0
                && std::io::Error::last_os_error().raw_os_error()
                    != Some(ERROR_PIPE_CONNECTED as i32)
            {
                return Err(format!(
                    "accept failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let sid = client_sid(raw)?;
            Ok((connected, sid))
        }
    }

    /// The SID of the account on the other end: the client's process, from the pipe, and that process's
    /// token. (Impersonation would be exact but the OS refuses it until the server has read a byte, and
    /// the identity is needed before any frame.) Residual: if the client exits and its PID is reused by
    /// another process in the microseconds between connect and this call, that process's SID is read; the
    /// DACL still limits who could have connected, so only another allowed account could be mistaken.
    fn client_sid(pipe: HANDLE) -> Result<String, String> {
        let mut pid = 0u32;
        // SAFETY: `pipe` is a connected server-side pipe handle.
        if unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) } == 0 {
            return Err("could not read the client's process".into());
        }
        // SAFETY: a plain open of a process by id; the handle is closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return Err("could not open the client's process".into());
        }
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `process` is open; `token` receives a handle closed below.
        let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
        // SAFETY: opened above and not used again.
        unsafe { CloseHandle(process) };
        if opened == 0 {
            return Err("could not open the client's token".into());
        }
        let result = token_user_sid(token);
        // SAFETY: `token` was opened above and is not used again.
        unsafe { CloseHandle(token) };
        result
    }

    fn token_user_sid(token: HANDLE) -> Result<String, String> {
        let mut need = 0u32;
        // SAFETY: a size query with a null buffer.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut need) };
        if need == 0 || need > 4096 {
            return Err("could not size the client's token".into());
        }
        // u64 words so the buffer is aligned for TOKEN_USER.
        let mut buf = vec![0u64; (need as usize).div_ceil(8)];
        // SAFETY: `buf` is at least `need` bytes.
        let ok = unsafe {
            GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), need, &mut need)
        };
        if ok == 0 {
            return Err("could not read the client's token".into());
        }
        // SAFETY: the call filled `buf` with a TOKEN_USER whose SID points inside it.
        let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid` is a valid SID inside `buf`.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
            return Err("could not format the client's SID".into());
        }
        // SAFETY: a NUL-terminated wide string from the call above, freed once.
        let s = unsafe {
            let mut len = 0;
            while *text.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text as _);
            s
        };
        Ok(s)
    }
}

#[cfg(windows)]
pub use imp::Listener;

/// The broker is Windows-only; elsewhere this refuses rather than pretends.
#[cfg(not(windows))]
pub struct Listener;
#[cfg(not(windows))]
impl Listener {
    pub fn new(_: &str, _: &[String]) -> Result<Self, String> {
        Err("the broker pipe is implemented for Windows only".into())
    }
    pub fn accept(&mut self) -> Result<(std::fs::File, String), String> {
        Err("the broker pipe is implemented for Windows only".into())
    }
}
