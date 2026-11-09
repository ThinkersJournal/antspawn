// SPDX-License-Identifier: MIT OR Apache-2.0
//! The cloud-init NoCloud seed a broker-made guest boots with (slice V3 of `docs/SANDBOX-BROKER.md`):
//! the three files `user-data`, `meta-data` and `network-config`, as text.
//!
//! Pure functions, tested as text on every CI leg. What is NOT shown: that a real cloud-init accepts
//! them, or that the ISO built from them (see [`crate::hyperv`]) is read by a guest. Both need V6/V4's
//! real guest.
//!
//! Nothing free-form reaches the files. The key is re-validated here (the broker validated it, this is
//! the backstop) and only its type and base64 blob are written, never the comment, so the value placed
//! in YAML is `ssh-ed25519` plus base64 characters and nothing that could end a string or start a line.

use crate::broker_proto::{valid_vm_id, validate_pubkey};
use std::net::Ipv4Addr;

/// The one account cloud-init makes in the guest.
pub const GUEST_USER: &str = "antspawn";

/// The three NoCloud files, `(file name, content)`.
///
/// # Errors
/// The id, key or an address is not acceptable.
pub fn files(
    id: &str,
    pubkey: &str,
    guest: &str,
    prefix: u8,
) -> Result<Vec<(&'static str, String)>, String> {
    if !valid_vm_id(id) {
        return Err("seed: not a valid VM id".into());
    }
    validate_pubkey(pubkey).map_err(|e| format!("seed: {e}"))?;
    let guest: Ipv4Addr = guest
        .parse()
        .map_err(|_| "seed: the guest address is not an IPv4 address".to_string())?;
    if guest.octets()[..2] != [10, 77] {
        return Err("seed: the guest address is outside 10.77.0.0/16".into());
    }
    if !(8..=30).contains(&prefix) {
        return Err("seed: prefix outside 8..=30".into());
    }
    // Type and blob only: the comment is the caller's text and goes nowhere.
    let key: Vec<&str> = pubkey.split(' ').take(2).collect();
    let key = key.join(" ");
    let user_data = format!(
        "#cloud-config\n\
         users:\n\
         \x20 - name: {GUEST_USER}\n\
         \x20   shell: /bin/bash\n\
         \x20   lock_passwd: true\n\
         \x20   sudo: ALL=(ALL) NOPASSWD:ALL\n\
         \x20   ssh_authorized_keys:\n\
         \x20     - {key}\n\
         ssh_pwauth: false\n\
         disable_root: true\n"
    );
    let meta_data = format!("instance-id: {id}\nlocal-hostname: {id}\n");
    // No gateway: the guest has no route off its switch, and the file must not invent one.
    let network_config = format!(
        "version: 2\n\
         ethernets:\n\
         \x20 nic0:\n\
         \x20   match:\n\
         \x20     name: \"e*\"\n\
         \x20   dhcp4: false\n\
         \x20   dhcp6: false\n\
         \x20   addresses:\n\
         \x20     - {guest}/{prefix}\n"
    );
    Ok(vec![
        ("user-data", user_data),
        ("meta-data", meta_data),
        ("network-config", network_config),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};

    fn key(comment: &str) -> String {
        let mut b = Vec::new();
        b.extend_from_slice(&11u32.to_be_bytes());
        b.extend_from_slice(b"ssh-ed25519");
        b.extend_from_slice(&32u32.to_be_bytes());
        b.extend_from_slice(&[7u8; 32]);
        format!("ssh-ed25519 {} {comment}", STANDARD.encode(b))
            .trim_end()
            .to_string()
    }

    fn get(v: &[(&'static str, String)], n: &str) -> String {
        v.iter().find(|(k, _)| *k == n).unwrap().1.clone()
    }

    #[test]
    fn the_three_files_carry_the_key_the_address_and_the_name() {
        let k = key("me@host");
        let v = files("antspawn-j1", &k, "10.77.3.2", 30).unwrap();
        let ud = get(&v, "user-data");
        assert!(ud.starts_with("#cloud-config\n"));
        let blob = k.split(' ').nth(1).unwrap();
        assert!(ud.contains(&format!("      - ssh-ed25519 {blob}\n")));
        assert!(ud.contains("ssh_pwauth: false") && ud.contains("lock_passwd: true"));
        assert_eq!(
            get(&v, "meta-data"),
            "instance-id: antspawn-j1\nlocal-hostname: antspawn-j1\n"
        );
        let nc = get(&v, "network-config");
        assert!(nc.contains("- 10.77.3.2/30"));
        assert!(
            !nc.contains("gateway"),
            "no route off the switch is invented"
        );
    }

    #[test]
    fn the_keys_comment_never_reaches_a_file() {
        let k = key("evil:{x}'#");
        let v = files("antspawn-j1", &k, "10.77.3.2", 30).unwrap();
        for (_, c) in &v {
            assert!(!c.contains("evil"), "comment leaked: {c}");
        }
    }

    #[test]
    fn a_bad_key_id_or_address_is_refused() {
        let k = key("");
        assert!(files("antspawn-j1", "ssh-ed25519 AAAA", "10.77.3.2", 30).is_err());
        assert!(files("antspawn-j1\nx", &k, "10.77.3.2", 30).is_err());
        assert!(files("my-laptop", &k, "10.77.3.2", 30).is_err());
        assert!(files("antspawn-j1", &k, "192.168.1.5", 30).is_err());
        assert!(files("antspawn-j1", &k, "10.77.3.2\n", 30).is_err());
        assert!(files("antspawn-j1", &k, "10.77.3.2", 31).is_err());
    }
}
