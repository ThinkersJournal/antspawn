// SPDX-License-Identifier: MIT OR Apache-2.0
//! The real Hyper-V [`Driver`] (slice V3 of `docs/SANDBOX-BROKER.md`).
//!
//! It acts through PowerShell, the same cmdlets `scripts/broker-v0.ps1` measured working for a
//! non-elevated Hyper-V Administrator with full control of its own disk directory. What is built
//! here without a host is the part that can be wrong in text: every script the driver would run is
//! produced by [`HyperV::script_*`] and tested as text, and the process itself is behind [`Runner`],
//! so the tests run on every CI leg with a recording runner. Nothing here has touched Hyper-V.
//!
//! Rules the scripts keep:
//! - a script goes to PowerShell on stdin, never on its command line, so nothing a caller supplied
//!   is parsed by a shell;
//! - the only values placed in a script are resolved ones (profile sizes, a slot's switch name, the
//!   configured directories) and one validated name; each goes in as a single-quoted PowerShell
//!   literal after [`quote`] refused anything that could end the literal or start a new line;
//! - `delete` is idempotent and removes only what carries the `antspawn-` prefix.

use crate::broker::{Driver, VmSpec};
use crate::broker_proto::{VM_PREFIX, valid_vm_id};

/// Runs one PowerShell script and returns its standard output.
pub trait Runner {
    /// # Errors
    /// The process could not start, or exited non-zero (the text carries its stderr).
    fn run(&self, script: &str) -> Result<String, String>;
}

/// Where the driver keeps things. Set by whoever starts the broker, never by a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperVConfig {
    /// Folder of read-only base disks, one `<image>.vhdx` per offered image.
    pub base_dir: String,
    /// Folder the broker has full control of: differencing disks and VM files.
    pub vm_dir: String,
    /// Expected SHA-256 (64 hex digits) of each offered image's base disk. An image with no entry
    /// cannot be created: a base nobody pinned is not one the broker will boot from.
    pub base_hashes: std::collections::BTreeMap<String, String>,
}

impl HyperVConfig {
    /// # Errors
    /// A hash is not 64 hex digits.
    pub fn check(&self) -> Result<(), String> {
        for (image, h) in &self.base_hashes {
            if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("hash for image {image} is not 64 hex digits"));
            }
        }
        Ok(())
    }
}

pub struct HyperV<R: Runner> {
    runner: R,
    cfg: HyperVConfig,
}

/// Single-quote `s` as a PowerShell literal.
///
/// # Errors
/// Empty, or contains a control character (a newline would end the statement) or a character
/// outside printable ASCII. A quote is doubled, which is how a single-quoted literal holds one.
pub fn quote(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err("empty value".into());
    }
    if let Some(c) = s.chars().find(|c| !(' '..='~').contains(c)) {
        return Err(format!("character {:?} not allowed in a script value", c));
    }
    Ok(format!("'{}'", s.replace('\'', "''")))
}

fn join(dir: &str, leaf: &str) -> String {
    format!("{}\\{}", dir.trim_end_matches('\\'), leaf)
}

impl<R: Runner> HyperV<R> {
    pub fn new(runner: R, cfg: HyperVConfig) -> Self {
        Self { runner, cfg }
    }

    fn disk(&self, id: &str) -> String {
        join(&self.cfg.vm_dir, &format!("{id}.vhdx"))
    }

    fn named(vm: &str) -> Result<String, String> {
        if valid_vm_id(vm) {
            quote(vm)
        } else {
            Err(format!(
                "refusing a VM name without the {VM_PREFIX} prefix or with odd characters"
            ))
        }
    }

    /// Hash check of the base first (a mismatch throws before any disk or VM exists), then a
    /// differencing disk on the image's base, a generation 2 VM on the slot's switch, no
    /// checkpoints, only the heartbeat integration service. The VM is not started.
    ///
    /// # Errors
    /// A value could not be quoted.
    pub fn script_create(&self, spec: &VmSpec) -> Result<String, String> {
        let name = Self::named(&spec.id)?;
        let base = quote(&join(&self.cfg.base_dir, &format!("{}.vhdx", spec.image)))?;
        let disk = quote(&self.disk(&spec.id))?;
        let want = self
            .cfg
            .base_hashes
            .get(&spec.image)
            .ok_or_else(|| format!("no pinned hash for image {}", spec.image))?;
        self.cfg.check()?;
        let want = quote(&want.to_ascii_uppercase())?;
        let switch = quote(&spec.slot.switch)?;
        let mem = u64::from(spec.profile.memory_mb) * 1024 * 1024;
        Ok(format!(
            "$ErrorActionPreference='Stop'\n\
             if ((Get-FileHash -Algorithm SHA256 -LiteralPath {base}).Hash -ne {want}) {{ throw 'base image does not match its pinned SHA-256' }}\n\
             New-VHD -Path {disk} -ParentPath {base} -Differencing | Out-Null\n\
             New-VM -Name {name} -Generation 2 -MemoryStartupBytes {mem} -VHDPath {disk} -SwitchName {switch} -Path {vmdir} | Out-Null\n\
             Set-VMMemory -VMName {name} -DynamicMemoryEnabled $false\n\
             Set-VMProcessor -VMName {name} -Count {cpus} -ExposeVirtualizationExtensions $false\n\
             Get-VMIntegrationService -VMName {name} | Where-Object {{ $_.Name -ne 'Heartbeat' }} | Disable-VMIntegrationService\n\
             Set-VM -Name {name} -CheckpointType Disabled\n",
            vmdir = quote(&self.cfg.vm_dir)?,
            cpus = spec.profile.vcpus,
        ))
    }

    /// # Errors
    /// The name is not one of ours.
    pub fn script_start(&self, vm: &str) -> Result<String, String> {
        Ok(format!(
            "$ErrorActionPreference='Stop'\nStart-VM -Name {}\n",
            Self::named(vm)?
        ))
    }

    /// The power cut: no guest cooperation.
    ///
    /// # Errors
    /// The name is not one of ours.
    pub fn script_stop(&self, vm: &str) -> Result<String, String> {
        Ok(format!(
            "$ErrorActionPreference='Stop'\n$v = Get-VM -Name {n} -ErrorAction SilentlyContinue\nif ($v -and $v.State -ne 'Off') {{ Stop-VM -Name {n} -TurnOff -Force }}\n",
            n = Self::named(vm)?
        ))
    }

    /// Idempotent: a VM or disk that is already gone is success.
    ///
    /// # Errors
    /// The name is not one of ours.
    pub fn script_delete(&self, vm: &str) -> Result<String, String> {
        let n = Self::named(vm)?;
        let disk = quote(&self.disk(vm))?;
        Ok(format!(
            "$ErrorActionPreference='Stop'\n\
             $v = Get-VM -Name {n} -ErrorAction SilentlyContinue\n\
             if ($v) {{ if ($v.State -ne 'Off') {{ Stop-VM -Name {n} -TurnOff -Force }}; Remove-VM -Name {n} -Force }}\n\
             if (Test-Path -LiteralPath {disk}) {{ Remove-Item -LiteralPath {disk} -Force }}\n"
        ))
    }

    #[must_use]
    pub fn script_list() -> &'static str {
        "$ErrorActionPreference='Stop'\nGet-VM | ForEach-Object { $_.Name }\n"
    }
}

impl<R: Runner> Driver for HyperV<R> {
    fn create(&mut self, spec: &VmSpec) -> Result<(), String> {
        let s = self.script_create(spec)?;
        self.runner.run(&s).map(|_| ())
    }
    fn start(&mut self, vm: &str) -> Result<(), String> {
        let s = self.script_start(vm)?;
        self.runner.run(&s).map(|_| ())
    }
    fn stop(&mut self, vm: &str) -> Result<(), String> {
        let s = self.script_stop(vm)?;
        self.runner.run(&s).map(|_| ())
    }
    fn delete(&mut self, vm: &str) -> Result<(), String> {
        let s = self.script_delete(vm)?;
        self.runner.run(&s).map(|_| ())
    }
    fn list(&self) -> Result<Vec<String>, String> {
        let out = self.runner.run(Self::script_list())?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
    }
}

/// Runs scripts with `powershell.exe -NoProfile -NonInteractive -Command -`, script on stdin.
pub struct PowerShell;

impl Runner for PowerShell {
    fn run(&self, script: &str) -> Result<String, String> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start powershell.exe: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("no stdin")?
            .write_all(script.as_bytes())
            .map_err(|e| format!("writing the script: {e}"))?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("waiting: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "powershell exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::Slot;
    use crate::broker_proto::Profile;
    use std::cell::RefCell;

    struct Rec(RefCell<Vec<String>>, String);
    impl Runner for Rec {
        fn run(&self, script: &str) -> Result<String, String> {
            self.0.borrow_mut().push(script.to_string());
            Ok(self.1.clone())
        }
    }

    fn cfg() -> HyperVConfig {
        HyperVConfig {
            base_dir: r"C:\ProgramData\antspawn\base".into(),
            vm_dir: r"C:\ProgramData\antspawn\vms".into(),
            base_hashes: [("ubuntu-24".to_string(), "ab".repeat(32))].into(),
        }
    }
    fn spec() -> VmSpec {
        VmSpec {
            id: "antspawn-j1".into(),
            image: "ubuntu-24".into(),
            profile: Profile {
                name: "small".into(),
                memory_mb: 1024,
                vcpus: 2,
                disk_gb: 8,
            },
            slot: Slot {
                switch: "antspawn-sw0".into(),
                host_address: "10.77.0.1".into(),
                guest_address: "10.77.0.2".into(),
            },
            pubkey: "ssh-ed25519 AAAA".into(),
        }
    }
    fn hv(out: &str) -> HyperV<Rec> {
        HyperV::new(Rec(RefCell::new(vec![]), out.into()), cfg())
    }

    #[test]
    fn quote_doubles_a_quote_and_refuses_a_newline() {
        assert_eq!(quote("a'b").unwrap(), "'a''b'");
        assert!(quote("a\nb").is_err());
        assert!(quote("a\u{e9}").is_err());
        assert!(quote("").is_err());
    }

    #[test]
    fn create_script_uses_resolved_values_and_no_pubkey() {
        let s = hv("").script_create(&spec()).unwrap();
        assert!(s.contains("-Differencing"));
        assert!(s.contains(r"'C:\ProgramData\antspawn\base\ubuntu-24.vhdx'"));
        assert!(s.contains(&format!("-MemoryStartupBytes {}", 1024u64 * 1024 * 1024)));
        assert!(s.contains("-Count 2"));
        assert!(s.contains("-SwitchName 'antspawn-sw0'"));
        assert!(s.contains("-CheckpointType Disabled"));
        assert!(
            !s.contains("ssh-ed25519"),
            "the key is not in the script: the seed ISO is not built yet"
        );
    }

    #[test]
    fn the_hash_is_checked_before_any_disk_is_made() {
        let s = hv("").script_create(&spec()).unwrap();
        let h = s.find("Get-FileHash").unwrap();
        assert!(h < s.find("New-VHD").unwrap());
        assert!(s.contains(&format!("'{}'", "AB".repeat(32))));
        assert!(s.contains("throw"));
    }

    #[test]
    fn an_image_without_a_pinned_hash_or_with_a_bad_one_is_refused() {
        let mut sp = spec();
        sp.image = "other".into();
        assert!(
            hv("")
                .script_create(&sp)
                .unwrap_err()
                .contains("no pinned hash")
        );
        let mut d = hv("");
        d.cfg.base_hashes.insert("ubuntu-24".into(), "zz".into());
        assert!(d.create(&spec()).is_err());
        assert!(d.runner.0.borrow().is_empty());
    }

    #[test]
    fn a_name_without_our_prefix_is_refused_before_anything_runs() {
        let mut d = hv("");
        assert!(d.delete("my-laptop-vm").is_err());
        assert!(d.stop("antspawn-x'; calc").is_err());
        assert!(d.runner.0.borrow().is_empty());
    }

    #[test]
    fn delete_is_idempotent_by_construction() {
        let s = hv("").script_delete("antspawn-j1").unwrap();
        assert!(s.contains("if ($v)"));
        assert!(s.contains("Test-Path -LiteralPath"));
        assert!(s.contains("Remove-VM"));
    }

    #[test]
    fn a_hostile_image_or_switch_cannot_end_the_literal() {
        let mut sp = spec();
        sp.image = "x'; Remove-VM *; '".into();
        assert!(
            hv("").script_create(&sp).is_err(),
            "an unpinned image never gets a script"
        );
        let mut d = hv("");
        d.cfg.base_hashes.insert(sp.image.clone(), "ab".repeat(32));
        let s = d.script_create(&sp).unwrap();
        assert!(
            s.contains("x''; Remove-VM *; ''"),
            "quote doubled inside one literal"
        );
        sp.slot.switch = "a\nRemove-VM *".into();
        assert!(d.script_create(&sp).is_err());
    }

    #[test]
    fn list_splits_lines_and_drops_blanks() {
        let d = hv("antspawn-a\r\n\r\nmy-laptop-vm\n");
        assert_eq!(d.list().unwrap(), vec!["antspawn-a", "my-laptop-vm"]);
    }
}
