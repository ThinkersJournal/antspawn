// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Windows Sandbox broker's configuration file (Track 2 of `docs/SANDBOX-BROKER.md`): who may
//! connect, and which fixed `.wsb` templates a client may ask for by name. Written by the operator,
//! read once at start, never from a client. Unknown keys are errors.
//!
//! ```toml
//! pipe = "antspawn-sandbox-broker"
//! allow_sids = ["S-1-5-21-..."]
//!
//! [[profiles]]
//! name = "default"
//! template = 'C:\antspawn\sandbox\default.wsb'
//!
//! [limits]            # optional
//! creates_per_minute = 4
//! max_lifetime_s = 3600
//! ```
//!
//! There is no `max_total`: Windows Sandbox runs one instance at a time and the broker forces that.
//! The template is read when a sandbox starts, not here; this only checks the path's shape.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::broker_config::absolute_path;
use crate::broker_proto::{Limits, Policy, valid_name};
use crate::session_host::valid_sid;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileRow {
    name: String,
    template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitsRow {
    creates_per_minute: Option<usize>,
    max_lifetime_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    pipe: String,
    allow_sids: Vec<String>,
    profiles: Vec<ProfileRow>,
    #[serde(default)]
    limits: Option<LimitsRow>,
}

/// A checked configuration, ready to start a sandbox broker from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxConfig {
    pub pipe: String,
    pub allow_sids: Vec<String>,
    pub policy: Policy,
    pub limits: Limits,
    /// profile name -> the absolute path of its fixed `.wsb` file.
    pub templates: BTreeMap<String, PathBuf>,
}

fn bad(what: &str) -> String {
    format!("sandbox-broker.toml: {what}")
}

impl SandboxConfig {
    /// # Errors
    /// Not TOML of this shape, an unknown key, or a value outside its bounds (the message names the
    /// field, never echoes a value).
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let f: File = toml::from_str(text).map_err(|e| format!("sandbox-broker.toml: {e}"))?;
        f.check()
    }
}

impl File {
    fn check(self) -> Result<SandboxConfig, String> {
        if self.pipe.is_empty()
            || self.pipe.len() > 64
            || !self
                .pipe
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(bad("pipe must be 1..=64 of a-z A-Z 0-9 - _"));
        }
        if self.allow_sids.is_empty()
            || self.allow_sids.len() > 16
            || !self.allow_sids.iter().all(|s| valid_sid(s))
        {
            return Err(bad("allow_sids needs 1..=16 well-formed SIDs"));
        }
        if self.profiles.is_empty() || self.profiles.len() > 16 {
            return Err(bad("profiles needs 1..=16 rows"));
        }
        let mut seen = BTreeSet::new();
        let mut templates = BTreeMap::new();
        for p in self.profiles {
            if !valid_name(&p.name) || !seen.insert(p.name.clone()) {
                return Err(bad("a profile name is not a plain name or is repeated"));
            }
            absolute_path(&p.template, "a profile template")?;
            if !p.template.to_ascii_lowercase().ends_with(".wsb") {
                return Err(bad("a profile template must be a .wsb file"));
            }
            templates.insert(p.name, PathBuf::from(p.template));
        }

        let mut limits = Limits::default();
        if let Some(l) = self.limits {
            if let Some(v) = l.creates_per_minute {
                limits.creates_per_minute = v;
            }
            if let Some(v) = l.max_lifetime_s {
                limits.max_lifetime = Duration::from_secs(v);
            }
        }
        if limits.creates_per_minute == 0
            || limits.max_lifetime < Duration::from_secs(60)
            || limits.max_lifetime > Duration::from_secs(24 * 3600)
        {
            return Err(bad(
                "limits: creates_per_minute >= 1, max_lifetime_s 60..=86400",
            ));
        }
        let policy = Policy {
            images: Vec::new(),
            profiles: Vec::new(),
            sandbox_profiles: templates.keys().cloned().collect(),
        };
        Ok(SandboxConfig {
            pipe: self.pipe,
            allow_sids: self.allow_sids,
            policy,
            limits,
            templates,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
pipe = "antspawn-sandbox-broker"
allow_sids = ["S-1-5-21-1-2-3-1001"]

[[profiles]]
name = "default"
template = 'C:\antspawn\sandbox\default.wsb'
"#;

    #[test]
    fn a_good_config_offers_exactly_its_profiles() {
        let c = SandboxConfig::from_toml(GOOD).unwrap();
        assert_eq!(c.policy.sandbox_profiles, vec!["default".to_string()]);
        assert!(c.policy.images.is_empty() && c.policy.profiles.is_empty());
        assert_eq!(
            c.templates["default"],
            PathBuf::from(r"C:\antspawn\sandbox\default.wsb")
        );
    }

    fn refused(text: &str, field: &str) {
        let e = SandboxConfig::from_toml(text).unwrap_err();
        assert!(e.contains(field), "{e}");
    }

    #[test]
    fn bad_values_are_refused_naming_the_field() {
        refused(&GOOD.replace("antspawn-sandbox-broker", "a b"), "pipe");
        refused(
            &GOOD.replace("S-1-5-21-1-2-3-1001", "Everyone"),
            "allow_sids",
        );
        refused(&GOOD.replace("default\"", "../x\""), "profile name");
        refused(
            &GOOD.replace(r"C:\antspawn\sandbox\default.wsb", "default.wsb"),
            "template",
        );
        refused(
            &GOOD.replace(r"C:\antspawn\sandbox\default.wsb", r"C:\a\..\b.wsb"),
            "template",
        );
        refused(
            &GOOD.replace(r"default.wsb", r"default.exe"),
            "must be a .wsb",
        );
        refused(&GOOD.replace("allow_sids", "allow"), "allow");
    }

    #[test]
    fn a_repeated_profile_and_bad_limits_are_refused() {
        let twice = format!("{GOOD}\n[[profiles]]\nname = \"default\"\ntemplate = 'C:\\x.wsb'\n");
        refused(&twice, "repeated");
        refused(
            &format!("{GOOD}\n[limits]\nmax_lifetime_s = 5\n"),
            "max_lifetime_s",
        );
        refused(&format!("{GOOD}\n[limits]\nmax_total = 3\n"), "max_total");
        refused(&GOOD.replace("[[profiles]]", "[[nothing]]"), "profiles");
    }
}
