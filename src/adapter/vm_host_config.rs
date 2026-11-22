// SPDX-License-Identifier: MIT OR Apache-2.0
//! `vm-host.toml`, beside the adapter config: where the runner base image and the VMs live (ROADMAP row 40). Read only when a group
//! names the `hyperv-vm` launcher. Set by whoever starts the adapter, never by a job. It holds paths and a hash, no secret: the guest
//! password is a DPAPI file (`guest_credential`), never here.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::vm_backend::VmHostConfig;

fn memory() -> u32 {
    4096
}
fn vcpus() -> u8 {
    2
}
fn runner_dir() -> String {
    r"C:\runner".into()
}
fn ready_s() -> u64 {
    300
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmHostFile {
    /// The read-only base disk, full path.
    pub base: String,
    /// Its SHA-256 (64 hex digits). A base nobody pinned is not booted from.
    pub base_sha256: String,
    /// Folder the adapter's account has full control of (differencing disks, VM files).
    pub vm_dir: String,
    /// The pool switch's name.
    pub switch: String,
    #[serde(default = "memory")]
    pub memory_mb: u32,
    #[serde(default = "vcpus")]
    pub vcpus: u8,
    /// The runner folder in the guest (holds `run.cmd`).
    #[serde(default = "runner_dir")]
    pub runner_dir: String,
    /// Longest the guest may take to answer PowerShell Direct after start.
    #[serde(default = "ready_s")]
    pub ready_s: u64,
}

/// `vm-host.toml` beside `config`.
pub fn path_for(config: &Path) -> PathBuf {
    config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("vm-host.toml")
}

impl VmHostFile {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let f: VmHostFile = toml::from_str(text).map_err(|e| e.to_string())?;
        f.validate()?;
        Ok(f)
    }

    fn validate(&self) -> Result<(), String> {
        if self.base_sha256.len() != 64 || !self.base_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("base_sha256 must be 64 hex digits".into());
        }
        for (k, v) in [
            ("base", &self.base),
            ("vm_dir", &self.vm_dir),
            ("switch", &self.switch),
            ("runner_dir", &self.runner_dir),
        ] {
            if v.trim().is_empty() {
                return Err(format!("{k} is empty"));
            }
        }
        for (k, v) in [("base", &self.base), ("vm_dir", &self.vm_dir)] {
            if !Path::new(v).is_absolute() {
                return Err(format!("{k} must be an absolute path"));
            }
        }
        if !(512..=65536).contains(&self.memory_mb) {
            return Err("memory_mb must be 512..=65536".into());
        }
        if !(1..=32).contains(&self.vcpus) {
            return Err("vcpus must be 1..=32".into());
        }
        if !(10..=1800).contains(&self.ready_s) {
            return Err("ready_s must be 10..=1800".into());
        }
        Ok(())
    }

    /// The host configuration `ScriptHost` takes.
    pub fn host(&self, guest_credential: PathBuf) -> VmHostConfig {
        VmHostConfig {
            base: self.base.clone(),
            base_sha256: self.base_sha256.clone(),
            vm_dir: self.vm_dir.clone(),
            switch: self.switch.clone(),
            memory_mb: self.memory_mb,
            vcpus: self.vcpus,
            guest_credential,
        }
    }
}

/// Read and check the file beside `config`. A missing file is an error naming where it should be.
pub fn load(config: &Path) -> Result<VmHostFile, String> {
    let path = path_for(config);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "a `hyperv-vm` group needs {} ({}): base, base_sha256, vm_dir, switch",
            path.display(),
            e.kind()
        )
    })?;
    VmHostFile::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"
base = 'C:\vmimages\pilot\core-vendor-base.vhdx'
base_sha256 = "4E6F8F78DE851B9DCFB0685BBC5126E9726B63E0B74F3E34F8D8088E39EA6C0B"
vm_dir = 'C:\vmimages\pilot-run'
switch = "antspawn-pool-1"
"#;

    #[test]
    fn a_minimal_file_takes_the_defaults() {
        let f = VmHostFile::from_toml(OK).unwrap();
        assert_eq!((f.memory_mb, f.vcpus, f.ready_s), (4096, 2, 300));
        assert_eq!(f.runner_dir, r"C:\runner");
    }

    #[test]
    fn each_bad_value_is_refused_by_name() {
        for (from, to, what) in [
            ("4E6F8F78", "ZZ", "base_sha256"),
            ("antspawn-pool-1", "", "switch is empty"),
            (
                r"C:\vmimages\pilot-run",
                "rel/dir",
                "vm_dir must be an absolute",
            ),
        ] {
            let bad = if what == "base_sha256" {
                OK.replace(
                    "4E6F8F78DE851B9DCFB0685BBC5126E9726B63E0B74F3E34F8D8088E39EA6C0B",
                    "4E6F",
                )
            } else {
                OK.replace(from, to)
            };
            let e = VmHostFile::from_toml(&bad).unwrap_err();
            assert!(e.contains(what), "{what}: {e}");
        }
        // control: a key nobody declared is refused, a secret-looking one included
        let e = VmHostFile::from_toml(&format!("{OK}password = \"x\"\n")).unwrap_err();
        assert!(e.contains("password"), "{e}");
        assert!(
            VmHostFile::from_toml(OK).is_ok(),
            "control: the good file loads"
        );
    }

    #[test]
    fn a_missing_file_names_where_it_belongs() {
        let e = load(Path::new(r"C:\definitely\not\here\adapter.toml")).unwrap_err();
        assert!(e.contains("vm-host.toml") && e.contains("hyperv-vm"), "{e}");
    }
}
