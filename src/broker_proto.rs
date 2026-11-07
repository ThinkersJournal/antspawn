// SPDX-License-Identifier: MIT OR Apache-2.0
//! Request vocabulary and pure validation for the VM broker and the Windows Sandbox broker
//! (slices V1 and SB1 of `docs/SANDBOX-BROKER.md`).
//!
//! Pure: no pipe, no Hyper-V, no process, no account. A broker that holds rights its client lacks
//! has exactly one boundary, which is this file: what a request may say, and how many of them one
//! account may have. So the vocabulary is closed and small:
//!
//! - **No host paths.** A request names an `image` and a `profile` from sets the broker's setup
//!   chose, and carries one public key. There is no field that could hold a path, a script or XML,
//!   and every name must also pass [`valid_name`] (no separator, no dot-dot, no drive letter).
//! - **Unknown fields and unknown ops are errors**, so a later verb is refused by an older broker.
//! - **A key is one line of `ssh-ed25519 <base64> [comment]`**, nothing before it (an
//!   `authorized_keys` option such as `command=` would be an injection) and no control character.
//! - **A VM belongs to the connection that created it** ([`Ledger`]): another connection, even from
//!   the same account, may not stop, start or delete it, and counts, rates and lifetimes are
//!   enforced per account from the SID the broker read from the pipe, never from the request.
//!
//! Nothing here is exercised against a real peer: the pipe, the Hyper-V calls and the account are
//! later slices, and the sizes in [`Limits`] are choices, not measurements.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

/// Longest accepted public-key line, in bytes (an ed25519 line is about 100).
pub const MAX_PUBKEY: usize = 512;
/// Longest accepted name (image, profile, VM id).
pub const MAX_NAME: usize = 48;
/// Every VM the broker makes starts with this; it is also how orphans are found after a crash.
pub const VM_PREFIX: &str = "antspawn-";

/// What a client may ask for. Struct variants only: a unit variant of a tagged enum would ignore
/// unknown fields even with `deny_unknown_fields`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// A VM from a fixed image, sized by a named profile, reachable only with this key.
    Create {
        image: String,
        profile: String,
        pubkey: String,
    },
    Stop {
        vm: String,
    },
    Start {
        vm: String,
    },
    Delete {
        vm: String,
    },
    /// The caller's own VMs only.
    List {},
    /// A Windows Sandbox from a fixed template (the Track 2 broker).
    SandboxStart {
        profile: String,
    },
}

/// A size a request may pick by name. Numbers never come from a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub memory_mb: u32,
    pub vcpus: u8,
    pub disk_gb: u32,
}

/// What this broker was set up to offer. Filled from the broker's own config, never from a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub images: Vec<String>,
    pub profiles: Vec<Profile>,
    pub sandbox_profiles: Vec<String>,
}

/// The caps. Defaults are a number chosen, not a measurement (63.2 GB RAM, 32 logical CPUs here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub max_total: usize,
    pub max_per_account: usize,
    pub creates_per_minute: usize,
    pub max_lifetime: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total: 4,
            max_per_account: 2,
            creates_per_minute: 4,
            max_lifetime: Duration::from_secs(4 * 3600),
        }
    }
}

/// Why a request was refused. Messages name the field, never its value: a rejected value is
/// untrusted text and need not be echoed anywhere.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("{0} is not a plain name (1..=48 of a-z 0-9 . _ -, starting with a letter or digit)")]
    BadName(&'static str),
    #[error("{0} is not one this broker offers")]
    NotOffered(&'static str),
    #[error("pubkey is not exactly one `ssh-ed25519 <base64> [comment]` line: {0}")]
    BadKey(&'static str),
    #[error("vm is not a name this broker could have made")]
    BadVmId,
    #[error("busy: the broker is at its total VM cap")]
    TotalCap,
    #[error("busy: this account is at its VM cap")]
    AccountCap,
    #[error("busy: this account is creating too fast")]
    Rate,
    #[error("that VM is not yours")]
    NotYours,
    #[error("no such VM")]
    Unknown,
    #[error("that VM name is already in use")]
    Duplicate,
}

/// One plain name: 1..=[`MAX_NAME`] of `a-z 0-9 . _ -`, first character a letter or digit, no `..`.
/// Refuses separators, drive letters, spaces and anything that could climb or name a stream.
pub fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    s.len() <= MAX_NAME
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && !s.contains("..")
}

/// A VM id the broker could have made: `antspawn-` then a plain name.
pub fn valid_vm_id(s: &str) -> bool {
    s.strip_prefix(VM_PREFIX).is_some_and(valid_name) && s.len() <= MAX_NAME
}

/// Exactly one `ssh-ed25519 <base64> [comment]` line whose blob really is an ed25519 key.
///
/// # Errors
/// [`Refusal::BadKey`] with the reason; the key itself is never echoed.
pub fn validate_pubkey(s: &str) -> Result<(), Refusal> {
    let bad = Refusal::BadKey;
    if s.is_empty() || s.len() > MAX_PUBKEY {
        return Err(bad("empty or too long"));
    }
    if s.chars().any(|c| c.is_control() || !c.is_ascii()) {
        return Err(bad(
            "a control or non-ASCII character (a second line, perhaps)",
        ));
    }
    let parts: Vec<&str> = s.split(' ').collect();
    if !(2..=3).contains(&parts.len()) {
        return Err(bad(
            "wrong number of fields (options before the type are refused)",
        ));
    }
    if parts[0] != "ssh-ed25519" {
        return Err(bad("the type is not ssh-ed25519"));
    }
    let blob = STANDARD
        .decode(parts[1])
        .map_err(|_| bad("the key is not base64"))?;
    // string "ssh-ed25519" (4+11) then string of 32 key bytes (4+32)
    let mut want = Vec::with_capacity(51);
    want.extend_from_slice(&11u32.to_be_bytes());
    want.extend_from_slice(b"ssh-ed25519");
    want.extend_from_slice(&32u32.to_be_bytes());
    if blob.len() != 51 || blob[..19] != want[..] {
        return Err(bad("the blob is not an ed25519 public key"));
    }
    if let Some(comment) = parts.get(2)
        && (comment.is_empty() || comment.len() > 64)
    {
        return Err(bad("the comment is empty or too long"));
    }
    Ok(())
}

impl Request {
    /// Everything that can be decided without state.
    ///
    /// # Errors
    /// The first [`Refusal`] found.
    pub fn validate(&self, policy: &Policy) -> Result<(), Refusal> {
        match self {
            Request::Create {
                image,
                profile,
                pubkey,
            } => {
                if !valid_name(image) {
                    return Err(Refusal::BadName("image"));
                }
                if !valid_name(profile) {
                    return Err(Refusal::BadName("profile"));
                }
                if !policy.images.iter().any(|i| i == image) {
                    return Err(Refusal::NotOffered("image"));
                }
                if !policy.profiles.iter().any(|p| &p.name == profile) {
                    return Err(Refusal::NotOffered("profile"));
                }
                validate_pubkey(pubkey)
            }
            Request::Stop { vm } | Request::Start { vm } | Request::Delete { vm } => {
                if valid_vm_id(vm) {
                    Ok(())
                } else {
                    Err(Refusal::BadVmId)
                }
            }
            Request::List {} => Ok(()),
            Request::SandboxStart { profile } => {
                if !valid_name(profile) {
                    return Err(Refusal::BadName("profile"));
                }
                if policy.sandbox_profiles.iter().any(|p| p == profile) {
                    Ok(())
                } else {
                    Err(Refusal::NotOffered("profile"))
                }
            }
        }
    }
}

struct Entry {
    conn: u64,
    account: String,
    created: Duration,
}

/// Who owns which VM, and the counts. Time is passed in (a monotonic offset), so it is testable
/// and the broker cannot be rushed or slowed by anything a client sends.
pub struct Ledger {
    limits: Limits,
    vms: BTreeMap<String, Entry>,
    creates: BTreeMap<String, VecDeque<Duration>>,
}

impl Ledger {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            vms: BTreeMap::new(),
            creates: BTreeMap::new(),
        }
    }

    /// Records a new VM for connection `conn`, whose pipe client the broker read as `account`
    /// (a SID string). Nothing here is read from the request.
    ///
    /// # Errors
    /// A cap, the rate, or a duplicate id; nothing is recorded then.
    pub fn try_create(
        &mut self,
        conn: u64,
        account: &str,
        vm: &str,
        now: Duration,
    ) -> Result<(), Refusal> {
        if !valid_vm_id(vm) {
            return Err(Refusal::BadVmId);
        }
        if self.vms.contains_key(vm) {
            return Err(Refusal::Duplicate);
        }
        if self.vms.len() >= self.limits.max_total {
            return Err(Refusal::TotalCap);
        }
        if self.vms.values().filter(|e| e.account == account).count() >= self.limits.max_per_account
        {
            return Err(Refusal::AccountCap);
        }
        let window = self.creates.entry(account.to_string()).or_default();
        while window
            .front()
            .is_some_and(|t| now.saturating_sub(*t) >= Duration::from_secs(60))
        {
            window.pop_front();
        }
        if window.len() >= self.limits.creates_per_minute {
            return Err(Refusal::Rate);
        }
        window.push_back(now);
        self.vms.insert(
            vm.to_string(),
            Entry {
                conn,
                account: account.to_string(),
                created: now,
            },
        );
        Ok(())
    }

    /// May `conn` act on `vm`? Only the connection that created it may, whatever the account.
    ///
    /// # Errors
    /// [`Refusal::Unknown`] or [`Refusal::NotYours`].
    pub fn authorize(&self, conn: u64, vm: &str) -> Result<(), Refusal> {
        match self.vms.get(vm) {
            None => Err(Refusal::Unknown),
            Some(e) if e.conn == conn => Ok(()),
            Some(_) => Err(Refusal::NotYours),
        }
    }

    /// The VMs `conn` owns (for `List`), sorted.
    #[must_use]
    pub fn owned_by(&self, conn: u64) -> Vec<String> {
        self.vms
            .iter()
            .filter(|(_, e)| e.conn == conn)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Forgets one VM (after the broker destroyed it). Returns whether it was recorded.
    pub fn remove(&mut self, vm: &str) -> bool {
        self.vms.remove(vm).is_some()
    }

    /// The connection went away: every VM it owned is due for destruction. Returns their ids and
    /// forgets them (the broker destroys them; the rate window is kept, so reconnecting does not
    /// reset it).
    pub fn disconnect(&mut self, conn: u64) -> Vec<String> {
        let gone = self.owned_by(conn);
        for vm in &gone {
            self.vms.remove(vm);
        }
        gone
    }

    /// VMs past the hard lifetime ceiling at `now`; forgotten, to be destroyed.
    pub fn expired(&mut self, now: Duration) -> Vec<String> {
        let gone: Vec<String> = self
            .vms
            .iter()
            .filter(|(_, e)| now.saturating_sub(e.created) >= self.limits.max_lifetime)
            .map(|(k, _)| k.clone())
            .collect();
        for vm in &gone {
            self.vms.remove(vm);
        }
        gone
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.vms.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vms.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 51 bytes: string "ssh-ed25519", string of 32 zero bytes.
    fn key_blob() -> String {
        let mut b = Vec::new();
        b.extend_from_slice(&11u32.to_be_bytes());
        b.extend_from_slice(b"ssh-ed25519");
        b.extend_from_slice(&32u32.to_be_bytes());
        b.extend_from_slice(&[7u8; 32]);
        STANDARD.encode(b)
    }

    fn good_key() -> String {
        format!("ssh-ed25519 {} runner@box", key_blob())
    }

    fn policy() -> Policy {
        Policy {
            images: vec!["debian-12".into(), "ubuntu-24".into()],
            profiles: vec![Profile {
                name: "small".into(),
                memory_mb: 4096,
                vcpus: 2,
                disk_gb: 20,
            }],
            sandbox_profiles: vec!["default".into()],
        }
    }

    fn create(image: &str, profile: &str, pubkey: &str) -> Request {
        Request::Create {
            image: image.into(),
            profile: profile.into(),
            pubkey: pubkey.into(),
        }
    }

    /// Requests a broker must refuse. Each is a known-bad control: a validator that allows one of
    /// them is a broker that hands a runner something it must not have.
    fn bad_requests() -> Vec<Request> {
        let k = good_key();
        vec![
            // host paths where a name goes
            create(r"C:\Windows\System32\evil.vhdx", "small", &k),
            create("..\\..\\base", "small", &k),
            create("../debian-12", "small", &k),
            create("debian-12", r"\\server\share", &k),
            create("debian-12/../ubuntu-24", "small", &k),
            // names that are plain but not offered
            create("windows-11", "small", &k),
            create("debian-12", "huge", &k),
            // key injection
            create("debian-12", "small", &format!("command=\"calc\" {k}")),
            create(
                "debian-12",
                "small",
                &format!("{k}\nssh-ed25519 AAAA other"),
            ),
            create("debian-12", "small", &format!("{k}\r\n")),
            create("debian-12", "small", "ssh-rsa AAAAB3NzaC1yc2E= x"),
            create("debian-12", "small", ""),
            create(
                "debian-12",
                "small",
                &format!("ssh-ed25519 {}", "A".repeat(600)),
            ),
            // a VM id that is not ours
            Request::Delete {
                vm: "my-laptop".into(),
            },
            Request::Stop {
                vm: "antspawn-../x".into(),
            },
            Request::Start {
                vm: r"antspawn-C:\x".into(),
            },
            Request::Delete { vm: String::new() },
            // sandbox
            Request::SandboxStart {
                profile: r"C:\x.wsb".into(),
            },
            Request::SandboxStart {
                profile: "not-offered".into(),
            },
        ]
    }

    fn all_refused(accepts: impl Fn(&Request) -> bool) -> bool {
        bad_requests().iter().all(|r| !accepts(r))
    }

    #[test]
    fn every_known_bad_request_is_refused_by_the_real_validator() {
        let p = policy();
        for r in bad_requests() {
            assert!(r.validate(&p).is_err(), "accepted: {r:?}");
        }
        assert!(all_refused(|r| r.validate(&p).is_ok()));
    }

    #[test]
    fn a_validator_that_allows_everything_fails_the_control() {
        // the control has teeth: a naive validator is caught by the same list
        assert!(!all_refused(|_| true));
        // and one that only checks the key but not names is caught too
        assert!(!all_refused(|r| match r {
            Request::Create { pubkey, .. } => validate_pubkey(pubkey).is_ok(),
            _ => false,
        }));
    }

    #[test]
    fn the_good_requests_are_accepted() {
        let p = policy();
        assert_eq!(
            create("debian-12", "small", &good_key()).validate(&p),
            Ok(())
        );
        assert_eq!(
            create("ubuntu-24", "small", &format!("ssh-ed25519 {}", key_blob())).validate(&p),
            Ok(()),
            "a key with no comment"
        );
        assert_eq!(
            Request::Delete {
                vm: "antspawn-j42".into()
            }
            .validate(&p),
            Ok(())
        );
        assert_eq!(Request::List {}.validate(&p), Ok(()));
        assert_eq!(
            Request::SandboxStart {
                profile: "default".into()
            }
            .validate(&p),
            Ok(())
        );
    }

    #[test]
    fn a_refusal_never_echoes_the_value() {
        let p = policy();
        let e = create(r"C:\secret\path", "small", &good_key())
            .validate(&p)
            .unwrap_err()
            .to_string();
        assert!(!e.contains("secret"), "{e}");
        let e = validate_pubkey("ssh-ed25519 !!notbase64!!")
            .unwrap_err()
            .to_string();
        assert!(!e.contains("notbase64"), "{e}");
    }

    #[test]
    fn a_blob_that_is_not_an_ed25519_key_is_refused_even_with_the_right_type_word() {
        let wrong = STANDARD.encode(b"not a key blob at all, but base64");
        assert!(validate_pubkey(&format!("ssh-ed25519 {wrong}")).is_err());
        // right shape, wrong inner type string
        let mut b = Vec::new();
        b.extend_from_slice(&11u32.to_be_bytes());
        b.extend_from_slice(b"ssh-ed25518");
        b.extend_from_slice(&32u32.to_be_bytes());
        b.extend_from_slice(&[0u8; 32]);
        assert!(validate_pubkey(&format!("ssh-ed25519 {}", STANDARD.encode(b))).is_err());
    }

    #[test]
    fn names_are_plain_or_refused() {
        for ok in ["a", "debian-12", "x.y_z-9"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-a",
            ".a",
            "A",
            "a b",
            "a/b",
            "a\\b",
            "a:b",
            "a..b",
            &"a".repeat(49),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(valid_vm_id("antspawn-abc"));
        assert!(!valid_vm_id("abc"));
        assert!(!valid_vm_id("antspawn-"));
    }

    #[test]
    fn unknown_ops_and_unknown_fields_are_errors_not_ignored() {
        assert!(serde_json::from_str::<Request>(r#"{"op":"snapshot","vm":"antspawn-a"}"#).is_err());
        assert!(
            serde_json::from_str::<Request>(r#"{"op":"stop","vm":"antspawn-a","force":true}"#)
                .is_err()
        );
        // a unit-like verb must also refuse an extra field (struct variant, by design)
        assert!(serde_json::from_str::<Request>(r#"{"op":"list","all":true}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"op":"list"}"#).is_ok());
        // a path field a client invents is an unknown field
        assert!(
            serde_json::from_str::<Request>(
                r#"{"op":"create","image":"debian-12","profile":"small","pubkey":"x","disk":"C:\\x.vhdx"}"#
            )
            .is_err()
        );
    }

    fn at(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn caps_hold_per_account_and_in_total() {
        let mut l = Ledger::new(Limits {
            max_total: 3,
            max_per_account: 2,
            creates_per_minute: 100,
            max_lifetime: at(3600),
        });
        assert_eq!(l.try_create(1, "S-A", "antspawn-a1", at(0)), Ok(()));
        assert_eq!(l.try_create(1, "S-A", "antspawn-a2", at(0)), Ok(()));
        assert_eq!(
            l.try_create(1, "S-A", "antspawn-a3", at(0)),
            Err(Refusal::AccountCap)
        );
        assert_eq!(l.try_create(2, "S-B", "antspawn-b1", at(0)), Ok(()));
        assert_eq!(
            l.try_create(3, "S-C", "antspawn-c1", at(0)),
            Err(Refusal::TotalCap)
        );
        assert_eq!(l.len(), 3, "a refused create records nothing");
    }

    #[test]
    fn the_per_minute_rate_is_per_account_and_not_reset_by_reconnecting() {
        let mut l = Ledger::new(Limits {
            max_total: 100,
            max_per_account: 100,
            creates_per_minute: 2,
            max_lifetime: at(3600),
        });
        assert_eq!(l.try_create(1, "S-A", "antspawn-a1", at(0)), Ok(()));
        assert_eq!(l.try_create(1, "S-A", "antspawn-a2", at(1)), Ok(()));
        assert_eq!(
            l.try_create(1, "S-A", "antspawn-a3", at(2)),
            Err(Refusal::Rate)
        );
        // a new connection from the same account does not get a fresh window
        l.disconnect(1);
        assert_eq!(
            l.try_create(9, "S-A", "antspawn-a4", at(3)),
            Err(Refusal::Rate)
        );
        // another account is unaffected
        assert_eq!(l.try_create(5, "S-B", "antspawn-b1", at(3)), Ok(()));
        // and the window does expire
        assert_eq!(l.try_create(9, "S-A", "antspawn-a5", at(61)), Ok(()));
    }

    #[test]
    fn only_the_creating_connection_may_act_on_a_vm_even_for_the_same_account() {
        let mut l = Ledger::new(Limits::default());
        l.try_create(1, "S-A", "antspawn-a1", at(0)).unwrap();
        assert_eq!(l.authorize(1, "antspawn-a1"), Ok(()));
        assert_eq!(l.authorize(2, "antspawn-a1"), Err(Refusal::NotYours));
        assert_eq!(l.authorize(1, "antspawn-zz"), Err(Refusal::Unknown));
        assert_eq!(l.owned_by(2), Vec::<String>::new());
        assert_eq!(l.owned_by(1), vec!["antspawn-a1".to_string()]);
    }

    #[test]
    fn a_disconnect_releases_exactly_that_connections_vms_and_a_duplicate_is_refused() {
        let mut l = Ledger::new(Limits::default());
        l.try_create(1, "S-A", "antspawn-a1", at(0)).unwrap();
        l.try_create(2, "S-B", "antspawn-b1", at(0)).unwrap();
        assert_eq!(
            l.try_create(2, "S-B", "antspawn-a1", at(1)),
            Err(Refusal::Duplicate)
        );
        assert_eq!(l.disconnect(1), vec!["antspawn-a1".to_string()]);
        assert_eq!(l.owned_by(2), vec!["antspawn-b1".to_string()]);
        assert!(l.disconnect(1).is_empty(), "nothing left to release twice");
    }

    #[test]
    fn a_vm_past_the_lifetime_ceiling_is_due_for_destruction() {
        let mut l = Ledger::new(Limits {
            max_lifetime: at(100),
            ..Limits::default()
        });
        l.try_create(1, "S-A", "antspawn-a1", at(0)).unwrap();
        l.try_create(1, "S-A", "antspawn-a2", at(50)).unwrap();
        assert!(l.expired(at(99)).is_empty());
        assert_eq!(l.expired(at(100)), vec!["antspawn-a1".to_string()]);
        assert_eq!(l.expired(at(200)), vec!["antspawn-a2".to_string()]);
        assert!(l.is_empty());
    }

    #[test]
    fn the_ledger_refuses_an_id_the_broker_could_not_have_made() {
        let mut l = Ledger::new(Limits::default());
        assert_eq!(l.try_create(1, "S-A", "evil", at(0)), Err(Refusal::BadVmId));
        assert!(l.is_empty());
    }
}
