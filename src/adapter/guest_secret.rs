// SPDX-License-Identifier: MIT OR Apache-2.0
//! The per-guest Administrator password for PowerShell Direct (ROADMAP row 40; PM ruling 2026-11-26, passed on by pm-ants):
//! **no long-lived guest credential.** Every runner VM gets a fresh random password (256 bits from the OS generator, never
//! reused across slots or builds) that
//! * is made here and handed to the host PowerShell on its **stdin**, never in a script, an argument, an environment block, a log,
//!   a status file, `with-secret` or a vault;
//! * is stored only as a DPAPI **CurrentUser** blob, one file per VM (`%LOCALAPPDATA%\antspawn\guests\<vm>.cred`), in a folder this
//!   adapter creates without elevation, with inheritance removed and an ACL of this user and SYSTEM only (set and read back by the
//!   provisioning script before any secret is written); removed when the VM is destroyed. Not the admin-only pool state folder
//!   (PM revised ruling 2026-11-26: the pool state stays Administrators and SYSTEM, written by the elevated `broker-pool.ps1`);
//! * reaches the guest over the VM bus (Hyper-V data exchange), where the base image's startup task `scripts/guest-bootstrap.ps1`
//!   sets it, and is taken off the bus once the guest has answered with it;
//! * is used only for PowerShell Direct to that one guest.
//!
//! The pilot/build tooling still uses the old fixed build password (`guest_credential`); that file never runs a job.

use std::path::{Path, PathBuf};

use ring::rand::{SecureRandom, SystemRandom};

use super::github::Secret;
use crate::broker_proto::valid_vm_id;
use crate::hyperv::quote;

/// Sub-folder of `%LOCALAPPDATA%\antspawn` that holds the per-VM blobs.
pub const SUB_DIR: &str = "guests";

/// `%LOCALAPPDATA%\antspawn\guests`, if `LOCALAPPDATA` is set.
pub fn default_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("antspawn").join(SUB_DIR))
}

/// A fresh password: 32 random bytes as 64 hex digits behind a fixed `Aa1-` so the local complexity rule (three of four character
/// classes) always holds. The prefix adds no secrecy and takes none away: all 256 bits are still random.
///
/// # Errors
/// The OS random generator failed.
pub fn fresh() -> Result<Secret, String> {
    let mut b = [0u8; 32];
    SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| "the OS random generator failed; no guest password was made".to_string())?;
    let mut s = String::from("Aa1-");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    b.fill(0);
    Ok(Secret::new(s))
}

/// The blob file of one VM. The name must be one of ours.
///
/// # Errors
/// `vm` is not a valid antspawn VM id, or `dir` is not absolute.
pub fn vm_file(dir: &Path, vm: &str) -> Result<PathBuf, String> {
    if !valid_vm_id(vm) {
        return Err(format!("`{vm}` is not an antspawn VM name"));
    }
    if !dir.is_absolute() && !is_windows_absolute(&dir.display().to_string()) {
        return Err(format!(
            "guest secret folder `{}` must be absolute",
            dir.display()
        ));
    }
    Ok(dir.join(format!("{vm}.cred")))
}

/// `C:\...` or `\\host\...`: a Windows host path judged the same on every platform (the Linux CI lesson).
fn is_windows_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || s.starts_with("\\\\")
}

/// Create `dir` if needed, then prove this process can create and remove a file in it. `Ok` is a fixed sentence. (The ACL is set and
/// read back by the provisioning script before it writes any secret; a folder this user can write is all `--check` needs.)
///
/// # Errors
/// A plain message for `--check`; none carries any secret.
pub fn check(dir: &Path) -> Result<String, String> {
    if !dir.is_absolute() {
        return Err(format!(
            "guest secret: folder `{}` must be absolute",
            dir.display()
        ));
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("guest secret: cannot create {} ({e})", dir.display()))?;
    let probe = dir.join(format!(".probe-{}", std::process::id()));
    std::fs::write(&probe, b"x").map_err(|e| {
        format!(
            "guest secret: this process cannot write {} ({e})",
            dir.display()
        )
    })?;
    let _ = std::fs::remove_file(&probe);
    Ok(format!(
        "guest secret: {} is writable by this process",
        dir.display()
    ))
}

/// PowerShell that defines `$cred` (user `Administrator`) from the VM's CurrentUser blob. The script holds the file's path only.
///
/// # Errors
/// A bad VM name or folder.
pub fn credential_preamble(dir: &Path, vm: &str) -> Result<String, String> {
    let p = quote(&vm_file(dir, vm)?.display().to_string())?;
    Ok(format!(
        "Add-Type -AssemblyName System.Security\n\
         $hex = (Get-Content -LiteralPath {p} -Raw).Trim()\n\
         $bytes = [byte[]]::new($hex.Length / 2)\n\
         for ($i = 0; $i -lt $bytes.Length; $i++) {{ $bytes[$i] = [Convert]::ToByte($hex.Substring(2 * $i, 2), 16) }}\n\
         $chars = [Text.Encoding]::UTF8.GetString([Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, 'CurrentUser')).ToCharArray()\n\
         $ss = New-Object System.Security.SecureString\n\
         foreach ($c in $chars) {{ $ss.AppendChar($c) }}\n\
         $ss.MakeReadOnly(); [Array]::Clear($chars, 0, $chars.Length)\n\
         $cred = New-Object System.Management.Automation.PSCredential('Administrator', $ss)\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_builds_get_two_different_passwords_of_full_strength() {
        let a = fresh().unwrap();
        let b = fresh().unwrap();
        assert_ne!(a.expose(), b.expose(), "a password was reused");
        for s in [&a, &b] {
            let p = s.expose();
            assert!(p.starts_with("Aa1-") && p.len() == 4 + 64, "{}", p.len());
            assert!(p[4..].bytes().all(|c| c.is_ascii_hexdigit()));
        }
        // control: many draws, no repeat (a constant generator would fail the pair above, a tiny space would fail here)
        let many: std::collections::HashSet<String> = (0..200)
            .map(|_| fresh().unwrap().expose().to_string())
            .collect();
        assert_eq!(many.len(), 200);
    }

    #[test]
    fn the_default_folder_is_under_local_app_data_not_the_admin_only_state_folder() {
        let d = default_dir().map(|p| p.display().to_string().to_ascii_lowercase());
        if let Some(d) = d {
            assert!(d.ends_with(r"antspawn\guests"), "{d}");
            assert!(!d.contains("antspawn-pool"), "{d}");
        }
    }

    #[test]
    fn a_blob_name_is_per_vm_and_only_for_our_vms() {
        let d = Path::new(r"C:\Users\x\AppData\Local\antspawn\guests");
        let f = vm_file(d, "antspawn-run-g-0").unwrap();
        assert!(f.display().to_string().ends_with("antspawn-run-g-0.cred"));
        assert_ne!(f, vm_file(d, "antspawn-run-g-1").unwrap());
        assert!(vm_file(d, "not-ours").is_err());
        assert!(vm_file(d, "antspawn-run-..\\x").is_err());
        assert!(vm_file(Path::new("guest"), "antspawn-run-g-0").is_err());
    }

    #[test]
    fn the_preamble_reads_a_local_machine_blob_and_holds_no_password() {
        let d = Path::new(r"C:\Users\x\AppData\Local\antspawn\guests");
        let s = credential_preamble(d, "antspawn-run-g-0").unwrap();
        assert!(
            s.contains("antspawn-run-g-0.cred") && s.contains("'CurrentUser'"),
            "{s}"
        );
        assert!(!s.contains("Aa1-"), "{s}");
        assert!(credential_preamble(d, "x").is_err());
    }

    #[test]
    fn check_makes_the_folder_without_elevation_and_needs_it_writable_and_absolute() {
        assert!(check(Path::new("rel")).is_err());
        let d = std::env::temp_dir().join(format!("antspawn-gs-{}", std::process::id()));
        assert!(!d.exists());
        assert!(check(&d).unwrap().contains("writable"));
        assert!(
            std::fs::read_dir(&d).unwrap().next().is_none(),
            "probe left behind"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
