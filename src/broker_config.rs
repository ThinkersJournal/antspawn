// SPDX-License-Identifier: MIT OR Apache-2.0
//! The VM broker's configuration file (slice V3 of `docs/SANDBOX-BROKER.md`): what it offers, where
//! it keeps things, who may connect. Written by the elevated setup, read once at start, never from a
//! client. Everything is checked here so the broker either starts with a coherent, bounded
//! configuration or refuses, naming the field.
//!
//! ```toml
//! pipe = "antspawn-broker"
//! allow_sids = ["S-1-5-21-..."]
//!
//! [hyperv]
//! base_dir = 'C:\antspawn\base'
//! vm_dir = 'C:\antspawn\vms'
//! [hyperv.base_hashes]
//! debian-12 = "<64 hex digits>"
//!
//! [[profiles]]
//! name = "small"
//! memory_mb = 4096
//! vcpus = 2
//! disk_gb = 20
//!
//! [[slots]]
//! switch = "antspawn-sw1"
//! host_address = "10.77.1.1"
//! guest_address = "10.77.1.2"
//! ```
//!
//! The offered images are exactly the keys of `hyperv.base_hashes`: an image with no pinned hash is
//! not offered. Unknown keys are errors.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::time::Duration;

use serde::Deserialize;

use crate::broker::Slot;
use crate::broker_proto::{Limits, Policy, Profile, valid_name};
use crate::hyperv::{HyperVConfig, quote};
use crate::session_host::valid_sid;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct HyperVSection {
    base_dir: String,
    vm_dir: String,
    base_hashes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileRow {
    name: String,
    memory_mb: u32,
    vcpus: u8,
    disk_gb: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct SlotRow {
    switch: String,
    host_address: String,
    guest_address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitsRow {
    max_total: Option<usize>,
    max_per_account: Option<usize>,
    creates_per_minute: Option<usize>,
    max_lifetime_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    pipe: String,
    allow_sids: Vec<String>,
    hyperv: HyperVSection,
    profiles: Vec<ProfileRow>,
    slots: Vec<SlotRow>,
    #[serde(default)]
    limits: Option<LimitsRow>,
}

/// A checked configuration, ready to start a broker from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerConfig {
    pub pipe: String,
    pub allow_sids: Vec<String>,
    pub hyperv: HyperVConfig,
    pub policy: Policy,
    pub limits: Limits,
    pub slots: Vec<Slot>,
}

fn bad(what: &str) -> String {
    format!("broker.toml: {what}")
}

fn pool_address(s: &str, what: &str) -> Result<Ipv4Addr, String> {
    let a: Ipv4Addr = s
        .parse()
        .map_err(|_| bad(&format!("{what} is not an IPv4 address")))?;
    if a.octets()[..2] != [10, 77] {
        return Err(bad(&format!("{what} is outside 10.77.0.0/16")));
    }
    Ok(a)
}

pub(crate) fn absolute_path(s: &str, what: &str) -> Result<(), String> {
    // quote() refuses empty, control and non-ASCII text, the same test the driver applies later.
    let b = s.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    if !drive || s.contains("..") || quote(s).is_err() {
        return Err(bad(&format!(
            "{what} must be an absolute path like C:\\dir, plain ASCII, no `..`"
        )));
    }
    Ok(())
}

impl BrokerConfig {
    /// # Errors
    /// Not TOML of this shape, an unknown key, or any value outside its bounds (the message names the
    /// field, never echoes a value).
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let f: File = toml::from_str(text).map_err(|e| format!("broker.toml: {e}"))?;
        f.check()
    }
}

impl File {
    fn check(self) -> Result<BrokerConfig, String> {
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
        absolute_path(&self.hyperv.base_dir, "hyperv.base_dir")?;
        absolute_path(&self.hyperv.vm_dir, "hyperv.vm_dir")?;
        if self
            .hyperv
            .base_dir
            .eq_ignore_ascii_case(&self.hyperv.vm_dir)
        {
            return Err(bad("hyperv.base_dir and hyperv.vm_dir must differ"));
        }
        if self.hyperv.base_hashes.is_empty() {
            return Err(bad("hyperv.base_hashes offers no image"));
        }
        if !self.hyperv.base_hashes.keys().all(|k| valid_name(k)) {
            return Err(bad(
                "an image name in hyperv.base_hashes is not a plain name",
            ));
        }
        let hyperv = HyperVConfig {
            base_dir: self.hyperv.base_dir,
            vm_dir: self.hyperv.vm_dir,
            base_hashes: self.hyperv.base_hashes,
        };
        hyperv.check().map_err(|e| bad(&e))?;

        if self.profiles.is_empty() || self.profiles.len() > 16 {
            return Err(bad("profiles needs 1..=16 rows"));
        }
        let mut seen = BTreeSet::new();
        let mut profiles = Vec::new();
        for p in self.profiles {
            if !valid_name(&p.name) || !seen.insert(p.name.clone()) {
                return Err(bad("a profile name is not a plain name or is repeated"));
            }
            if !(256..=65536).contains(&p.memory_mb)
                || !(1..=16).contains(&p.vcpus)
                || !(1..=512).contains(&p.disk_gb)
            {
                return Err(bad(
                    "a profile is outside memory_mb 256..=65536, vcpus 1..=16, disk_gb 1..=512",
                ));
            }
            profiles.push(Profile {
                name: p.name,
                memory_mb: p.memory_mb,
                vcpus: p.vcpus,
                disk_gb: p.disk_gb,
            });
        }

        if self.slots.is_empty() || self.slots.len() > 32 {
            return Err(bad("slots needs 1..=32 rows"));
        }
        let mut used: BTreeSet<Ipv4Addr> = BTreeSet::new();
        let mut switches = BTreeSet::new();
        let mut slots = Vec::new();
        for s in self.slots {
            if !valid_name(&s.switch) || !switches.insert(s.switch.clone()) {
                return Err(bad("a slot switch is not a plain name or is repeated"));
            }
            let host = pool_address(&s.host_address, "a slot host_address")?;
            let guest = pool_address(&s.guest_address, "a slot guest_address")?;
            if !used.insert(host) || !used.insert(guest) {
                return Err(bad("a slot address is used twice"));
            }
            slots.push(Slot {
                switch: s.switch,
                host_address: host.to_string(),
                guest_address: guest.to_string(),
            });
        }

        let mut limits = Limits::default();
        if let Some(l) = self.limits {
            if let Some(v) = l.max_total {
                limits.max_total = v;
            }
            if let Some(v) = l.max_per_account {
                limits.max_per_account = v;
            }
            if let Some(v) = l.creates_per_minute {
                limits.creates_per_minute = v;
            }
            if let Some(v) = l.max_lifetime_s {
                limits.max_lifetime = Duration::from_secs(v);
            }
        }
        if !(1..=64).contains(&limits.max_total)
            || limits.max_per_account == 0
            || limits.max_per_account > limits.max_total
            || limits.creates_per_minute == 0
            || limits.max_lifetime < Duration::from_secs(60)
            || limits.max_lifetime > Duration::from_secs(24 * 3600)
        {
            return Err(bad(
                "limits: max_total 1..=64, 1 <= max_per_account <= max_total, creates_per_minute >= 1, max_lifetime_s 60..=86400",
            ));
        }
        let policy = Policy {
            images: hyperv.base_hashes.keys().cloned().collect(),
            profiles,
            sandbox_profiles: Vec::new(),
        };
        Ok(BrokerConfig {
            pipe: self.pipe,
            allow_sids: self.allow_sids,
            hyperv,
            policy,
            limits,
            slots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn good() -> String {
        format!(
            r#"
pipe = "antspawn-broker"
allow_sids = ["S-1-5-21-1-2-3-1001"]

[hyperv]
base_dir = 'C:\antspawn\base'
vm_dir = 'C:\antspawn\vms'
[hyperv.base_hashes]
debian-12 = "{HASH}"

[[profiles]]
name = "small"
memory_mb = 4096
vcpus = 2
disk_gb = 20

[[slots]]
switch = "antspawn-sw1"
host_address = "10.77.1.1"
guest_address = "10.77.1.2"
"#
        )
    }

    fn with(from: &str, to: &str) -> String {
        let g = good();
        assert_eq!(g.matches(from).count(), 1, "anchor {from:?}");
        g.replace(from, to)
    }

    #[test]
    fn a_good_file_loads_and_offers_exactly_the_pinned_images() {
        let c = BrokerConfig::from_toml(&good()).unwrap();
        assert_eq!(c.policy.images, vec!["debian-12".to_string()]);
        assert_eq!(c.policy.profiles[0].memory_mb, 4096);
        assert_eq!(c.slots[0].guest_address, "10.77.1.2");
        assert_eq!(c.limits, Limits::default());
        assert!(c.policy.sandbox_profiles.is_empty());
    }

    #[test]
    fn every_known_bad_file_is_refused_naming_its_field() {
        let cases = [
            (
                with("pipe = \"antspawn-broker\"", "pipe = \"a\\\\b\""),
                "pipe",
            ),
            (with("[\"S-1-5-21-1-2-3-1001\"]", "[]"), "allow_sids"),
            (with("S-1-5-21-1-2-3-1001", "S-1-5"), "allow_sids"),
            (
                with("S-1-5-21-1-2-3-1001", "S-1-5-18)(A;;GA;;;WD"),
                "allow_sids",
            ),
            (with("'C:\\antspawn\\base'", "'base'"), "base_dir"),
            (with("'C:\\antspawn\\base'", "'C:\\a\\..\\b'"), "base_dir"),
            (
                with("'C:\\antspawn\\vms'", "'C:\\antspawn\\base'"),
                "must differ",
            ),
            (with(HASH, "abc"), "64 hex"),
            (with("debian-12 =", "\"../x\" ="), "image name"),
            (
                with("memory_mb = 4096", "memory_mb = 1"),
                "profile is outside",
            ),
            (with("vcpus = 2", "vcpus = 0"), "profile is outside"),
            (with("\"10.77.1.1\"", "\"192.168.1.1\""), "10.77.0.0/16"),
            (with("\"10.77.1.2\"", "\"10.77.1.1\""), "used twice"),
            (with("\"10.77.1.2\"", "\"10.77.1.2\\n\""), "IPv4"),
            (format!("{}\nsurprise = 1\n", good()), "surprise"),
            (format!("{}\n[limits]\nmax_total = 0\n", good()), "limits"),
            (
                format!("{}\n[limits]\nmax_per_account = 9\n", good()),
                "limits",
            ),
            (
                format!("{}\n[limits]\nmax_lifetime_s = 5\n", good()),
                "limits",
            ),
        ];
        for (text, needle) in cases {
            let e = BrokerConfig::from_toml(&text).expect_err(needle);
            assert!(e.contains(needle), "{needle}: got {e}");
        }
    }

    #[test]
    fn a_limits_table_overrides_only_what_it_names() {
        let t = format!(
            "{}\n[limits]\nmax_total = 8\nmax_lifetime_s = 600\n",
            good()
        );
        let c = BrokerConfig::from_toml(&t).unwrap();
        assert_eq!(c.limits.max_total, 8);
        assert_eq!(c.limits.max_lifetime, Duration::from_secs(600));
        assert_eq!(c.limits.max_per_account, Limits::default().max_per_account);
    }

    #[test]
    fn a_repeated_profile_or_switch_is_refused() {
        let dup_profile = format!(
            "{}\n[[profiles]]\nname = \"small\"\nmemory_mb = 2048\nvcpus = 1\ndisk_gb = 10\n",
            good()
        );
        // [[slots]] follows [[profiles]] in `good()`, so append a second slot instead for the switch case.
        assert!(BrokerConfig::from_toml(&dup_profile).is_err());
        let dup_switch = format!(
            "{}\n[[slots]]\nswitch = \"antspawn-sw1\"\nhost_address = \"10.77.2.1\"\nguest_address = \"10.77.2.2\"\n",
            good()
        );
        let e = BrokerConfig::from_toml(&dup_switch).unwrap_err();
        assert!(e.contains("switch"), "{e}");
    }
}
