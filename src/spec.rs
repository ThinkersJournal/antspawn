// SPDX-License-Identifier: MIT OR Apache-2.0
//! The job spec: what one job needs, declared up front.
//!
//! A spec is TOML. Parsing is strict (unknown keys are errors), and [`JobSpec::validate`] refuses
//! what slice 1 does not implement: a spec that lists a `secrets` name is rejected rather than run
//! without it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::capability::Capability;

/// A complete job declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    pub job: Job,
    #[serde(default, rename = "mount")]
    pub mounts: Vec<Mount>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub needs: NeedsSpec,
    #[serde(default, rename = "output")]
    pub outputs: Vec<Output>,
}

/// The command and its environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub name: String,
    /// Program and arguments; the first element is the program.
    pub cmd: Vec<String>,
    /// Working directory inside the environment.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Secret **names** only, resolved at run time and never logged or written to env or files.
    /// Slice 1 implements none: a non-empty list is refused by [`JobSpec::validate`].
    #[serde(default)]
    pub secrets: Vec<String>,
}

/// How a host path is exposed to the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    /// Read-only view of a host path (source checkout, toolchain cache).
    Ro,
    /// Writable, host-visible. Rare; every use widens what the job can change.
    Rw,
    /// Writable and empty at start, discarded at the end. `host` is not used.
    Scratch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    /// Host path (absent for `scratch`).
    #[serde(default)]
    pub host: Option<PathBuf>,
    /// Path inside the environment.
    pub guest: String,
    pub mode: MountMode,
}

/// Resource ceilings. `None` means the launcher's default, not "unlimited".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default)]
    pub cpu: Option<u32>,
    #[serde(default)]
    pub ram_mb: Option<u64>,
    #[serde(default)]
    pub time_s: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    #[default]
    Off,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gpu {
    #[default]
    None,
    Cuda,
    Vulkan,
}

/// The `[needs]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedsSpec {
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub gpu: Gpu,
    /// Tools the environment must provide (informational in slice 1; the operator maps them).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Capabilities required beyond those implied by `network` and `gpu`. Defaults to the
    /// isolation set: a job gets a clean, contained environment unless it says otherwise.
    #[serde(default = "default_require")]
    pub require: Vec<Capability>,
    /// Launcher ids in order of importance; launchers not listed follow in registry order.
    #[serde(default)]
    pub prefer: Vec<String>,
}

impl Default for NeedsSpec {
    fn default() -> Self {
        NeedsSpec {
            network: Network::Off,
            gpu: Gpu::None,
            tools: Vec::new(),
            require: default_require(),
            prefer: Vec::new(),
        }
    }
}

fn default_require() -> Vec<Capability> {
    vec![
        Capability::NoReadOutside,
        Capability::NoWriteOutside,
        Capability::CleanSlateNextRun,
        Capability::KillsRunawayProcess,
    ]
}

/// A file or folder to bring back to the host after the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// Path inside the environment.
    pub guest: String,
    /// Name under the results folder on the host.
    pub name: String,
}

/// Why a spec was refused.
#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("spec is not valid TOML for a job: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("job.cmd is empty")]
    EmptyCmd,
    #[error("job.name is empty")]
    EmptyName,
    #[error(
        "job lists secrets ({0}); this version implements none and refuses rather than run without them"
    )]
    SecretsNotImplemented(String),
    #[error("mount `{guest}`: {why}")]
    BadMount { guest: String, why: &'static str },
    #[error("output name `{0}` must be a single path segment")]
    BadOutputName(String),
}

/// What a spec needs from a launcher, as a set of capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needs {
    pub capabilities: Vec<Capability>,
    pub prefer: Vec<String>,
}

impl JobSpec {
    /// Parse and validate in one step.
    pub fn from_toml(text: &str) -> Result<Self, SpecError> {
        let spec: JobSpec = toml::from_str(text)?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), SpecError> {
        if self.job.name.trim().is_empty() {
            return Err(SpecError::EmptyName);
        }
        if self.job.cmd.is_empty() || self.job.cmd[0].is_empty() {
            return Err(SpecError::EmptyCmd);
        }
        if !self.job.secrets.is_empty() {
            return Err(SpecError::SecretsNotImplemented(
                self.job.secrets.join(", "),
            ));
        }
        for m in &self.mounts {
            match (m.mode, &m.host) {
                (MountMode::Scratch, Some(_)) => {
                    return Err(SpecError::BadMount {
                        guest: m.guest.clone(),
                        why: "a scratch mount has no host path",
                    });
                }
                (MountMode::Ro | MountMode::Rw, None) => {
                    return Err(SpecError::BadMount {
                        guest: m.guest.clone(),
                        why: "ro and rw mounts need a host path",
                    });
                }
                _ => {}
            }
        }
        for o in &self.outputs {
            if o.name.is_empty() || o.name.contains(['/', '\\']) || o.name == ".." || o.name == "."
            {
                return Err(SpecError::BadOutputName(o.name.clone()));
            }
        }
        Ok(())
    }

    /// The capability set this spec requires: `require`, plus what `network` and `gpu` imply.
    pub fn needs(&self) -> Needs {
        let mut caps = self.needs.require.clone();
        if self.needs.network == Network::Off {
            caps.push(Capability::NetworkOff);
        }
        match self.needs.gpu {
            Gpu::None => {}
            Gpu::Cuda => caps.push(Capability::GpuCuda),
            Gpu::Vulkan => caps.push(Capability::GpuVulkan),
        }
        caps.sort();
        caps.dedup();
        Needs {
            capabilities: caps,
            prefer: self.needs.prefer.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
[job]
name = "slangc"
cmd = ["slangc", "--version"]
"#;

    #[test]
    fn minimal_spec_requires_isolation_and_network_off() {
        let spec = JobSpec::from_toml(MINIMAL).unwrap();
        let caps = spec.needs().capabilities;
        for c in [
            Capability::NoReadOutside,
            Capability::NoWriteOutside,
            Capability::CleanSlateNextRun,
            Capability::KillsRunawayProcess,
            Capability::NetworkOff,
        ] {
            assert!(caps.contains(&c), "missing {c}");
        }
        assert!(!caps.contains(&Capability::GpuCuda));
    }

    #[test]
    fn network_on_drops_the_network_requirement_and_gpu_adds_one() {
        let spec =
            JobSpec::from_toml("[job]\nname='a'\ncmd=['x']\n[needs]\nnetwork='on'\ngpu='cuda'\n")
                .unwrap();
        let caps = spec.needs().capabilities;
        assert!(!caps.contains(&Capability::NetworkOff));
        assert!(caps.contains(&Capability::GpuCuda));
    }

    #[test]
    fn secrets_are_refused_not_ignored() {
        let err =
            JobSpec::from_toml("[job]\nname='a'\ncmd=['x']\nsecrets=['TOKEN']\n").unwrap_err();
        assert!(matches!(err, SpecError::SecretsNotImplemented(ref s) if s == "TOKEN"));
        assert!(!err.to_string().contains("value"));
    }

    #[test]
    fn unknown_keys_and_unknown_capabilities_are_errors() {
        assert!(JobSpec::from_toml("[job]\nname='a'\ncmd=['x']\nbogus=1\n").is_err());
        assert!(
            JobSpec::from_toml("[job]\nname='a'\ncmd=['x']\n[needs]\nrequire=['isolation.typo']\n")
                .is_err()
        );
    }

    #[test]
    fn mounts_and_outputs_are_checked() {
        let scratch_with_host =
            "[job]\nname='a'\ncmd=['x']\n[[mount]]\nhost='C:/x'\nguest='/w'\nmode='scratch'\n";
        assert!(matches!(
            JobSpec::from_toml(scratch_with_host),
            Err(SpecError::BadMount { .. })
        ));
        let ro_without_host = "[job]\nname='a'\ncmd=['x']\n[[mount]]\nguest='/w'\nmode='ro'\n";
        assert!(matches!(
            JobSpec::from_toml(ro_without_host),
            Err(SpecError::BadMount { .. })
        ));
        let bad_out = "[job]\nname='a'\ncmd=['x']\n[[output]]\nguest='/o'\nname='../up'\n";
        assert!(matches!(
            JobSpec::from_toml(bad_out),
            Err(SpecError::BadOutputName(_))
        ));
    }

    #[test]
    fn empty_cmd_and_name_are_refused() {
        assert!(matches!(
            JobSpec::from_toml("[job]\nname='a'\ncmd=[]\n"),
            Err(SpecError::EmptyCmd)
        ));
        assert!(matches!(
            JobSpec::from_toml("[job]\nname=' '\ncmd=['x']\n"),
            Err(SpecError::EmptyName)
        ));
    }
}
