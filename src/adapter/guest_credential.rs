// SPDX-License-Identifier: MIT OR Apache-2.0
//! The guest Administrator password for PowerShell Direct (ROADMAP row 40; CireSnave ruled board 488 "A"): one fixed password
//! baked into the runner image, kept as a DPAPI user-scope file under `%LOCALAPPDATA%\antspawn` by
//! `scripts/store-guest-credential.ps1`. Only the writing account decrypts it.
//!
//! **The value never passes through this program's output, a log, an argument or an environment block.** [`check`] decrypts the
//! file only to prove it can be read and reports nothing about the content. The PowerShell that talks to a guest builds its
//! `PSCredential` itself from the same file ([`credential_preamble`]); the password stays inside that process.

use std::path::{Path, PathBuf};

use crate::hyperv::quote;

/// File name under `%LOCALAPPDATA%\antspawn`.
pub const FILE_NAME: &str = "guest-credential.dpapi";

/// `%LOCALAPPDATA%\antspawn\guest-credential.dpapi`, if `LOCALAPPDATA` is set.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("antspawn").join(FILE_NAME))
}

/// Can the file at `path` be read and decrypted as this user? `Ok` carries a fixed sentence, never a length or any part of the
/// password. The error is a plain message for `--check`.
pub fn check(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err(format!(
            "guest credential: path `{}` must be absolute",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|_| {
        format!(
            "guest credential: {} is missing or unreadable; CireSnave stores it once with scripts\\store-guest-credential.ps1 -Apply",
            path.display()
        )
    })?;
    read_plain(text.trim())
        .map(|()| format!("guest credential: {} decrypts as this user", path.display()))
}

#[cfg(windows)]
fn read_plain(hex: &str) -> Result<(), String> {
    use super::github_client::{dpapi, unhex};
    let blob = unhex(hex).ok_or("guest credential: the file is not a DPAPI hex blob")?;
    let plain = dpapi::unprotect(&blob).ok_or(
        "guest credential: the file does not decrypt as this user (it was written by another account)",
    )?;
    if plain.is_empty() {
        return Err("guest credential: the file holds an empty password".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn read_plain(_: &str) -> Result<(), String> {
    Err("guest credential: DPAPI exists only on Windows".into())
}

/// PowerShell that defines `$cred` (user `Administrator`) from the DPAPI file. The password is never in the script text: the
/// script holds only the file's path.
pub fn credential_preamble(path: &Path) -> Result<String, String> {
    let p = quote(&path.display().to_string())?;
    Ok(format!(
        "$cred = New-Object System.Management.Automation.PSCredential('Administrator', (ConvertTo-SecureString ((Get-Content -LiteralPath {p} -Raw).Trim())))\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antspawn-gc-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(FILE_NAME)
    }

    #[test]
    fn a_missing_file_fails_with_a_plain_message() {
        let p = tmp("missing");
        let e = check(&p).unwrap_err();
        assert!(e.starts_with("guest credential:"), "{e}");
        assert!(e.contains("store-guest-credential.ps1"), "{e}");
    }

    #[test]
    fn a_relative_path_is_refused() {
        assert!(check(Path::new("guest-credential.dpapi")).is_err());
    }

    #[test]
    fn the_preamble_names_the_file_and_holds_no_password() {
        let s = credential_preamble(Path::new(r"C:\Users\x\a'b\guest-credential.dpapi")).unwrap();
        assert!(
            s.contains("'C:\\Users\\x\\a''b\\guest-credential.dpapi'"),
            "{s}"
        );
        assert!(s.contains("ConvertTo-SecureString"));
        assert!(!s.contains("-AsPlainText"), "{s}");
    }

    #[cfg(windows)]
    mod dpapi_files {
        use super::*;
        use crate::adapter::github_client::dpapi;

        fn hex(b: &[u8]) -> String {
            b.iter().map(|x| format!("{x:02x}")).collect()
        }
        /// A password as `ConvertFrom-SecureString` stores it: UTF-16LE plaintext under DPAPI, hex-encoded.
        fn write(p: &Path, pw: &str) {
            let utf16: Vec<u8> = pw.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let blob = dpapi::protect(&utf16).unwrap();
            std::fs::write(p, hex(&blob)).unwrap();
        }

        #[test]
        fn the_file_is_not_plaintext_and_check_reports_none_of_it() {
            let p = tmp("ok");
            let pw = "Zq9-unmistakable-Pw-7731";
            write(&p, pw);
            let on_disk = std::fs::read_to_string(&p).unwrap();
            assert!(!on_disk.contains(pw), "plaintext on disk");
            // UTF-16LE plaintext would not match either; the control is that a plaintext file would have matched the first assert.
            let utf16: String = pw
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .map(|b| format!("{b:02x}"))
                .collect();
            assert!(!on_disk.contains(&utf16), "UTF-16 plaintext on disk");
            let said = check(&p).unwrap();
            assert!(!said.contains(pw) && !said.contains("7731"), "{said}");
        }

        #[test]
        fn plaintext_and_empty_files_fail_the_check() {
            let p = tmp("bad");
            std::fs::write(&p, "not-a-blob").unwrap();
            assert!(check(&p).is_err());
            std::fs::write(&p, "").unwrap();
            assert!(check(&p).is_err());
            write(&p, "");
            assert!(check(&p).is_err(), "empty password accepted");
        }
    }
}
