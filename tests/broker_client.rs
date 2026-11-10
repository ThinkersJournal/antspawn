// SPDX-License-Identifier: MIT OR Apache-2.0
//! The broker client over the real broker pipe, the real framing and the real broker core, with a fake
//! driver in place of Hyper-V (slice V4). Shown: the five verbs, a stop and a start on the same VM,
//! the lifetime tied to the connection (a vanished client's VM is destroyed), a refusal arriving as an
//! error. NOT shown: real Hyper-V, a client from another account, the PersistAnt power-cut.
#![cfg(windows)]

use antspawn::broker::{Broker, Driver, Slot, VmSpec};
use antspawn::broker_client::connect;
use antspawn::broker_pipe::Listener;
use antspawn::broker_proto::{Limits, Policy, Profile};
use antspawn::broker_serve::serve_owned;
use base64::Engine;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<(BTreeSet<String>, Vec<String>)>>);

impl Driver for Fake {
    fn create(&mut self, s: &VmSpec) -> Result<(), String> {
        let mut w = self.0.lock().unwrap();
        w.0.insert(s.id.clone());
        w.1.push(format!("create {}", s.id));
        Ok(())
    }
    fn start(&mut self, v: &str) -> Result<(), String> {
        self.0.lock().unwrap().1.push(format!("start {v}"));
        Ok(())
    }
    fn stop(&mut self, v: &str) -> Result<(), String> {
        self.0.lock().unwrap().1.push(format!("stop {v}"));
        Ok(())
    }
    fn delete(&mut self, v: &str) -> Result<(), String> {
        let mut w = self.0.lock().unwrap();
        w.0.remove(v);
        w.1.push(format!("delete {v}"));
        Ok(())
    }
    fn list(&self) -> Result<Vec<String>, String> {
        Ok(self.0.lock().unwrap().0.iter().cloned().collect())
    }
}

fn me() -> String {
    let out = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.trim()
        .rsplit(',')
        .next()
        .unwrap()
        .trim_matches('"')
        .to_string()
}

fn key() -> String {
    let mut b = Vec::new();
    b.extend_from_slice(&11u32.to_be_bytes());
    b.extend_from_slice(b"ssh-ed25519");
    b.extend_from_slice(&32u32.to_be_bytes());
    b.extend_from_slice(&[5u8; 32]);
    format!(
        "ssh-ed25519 {}",
        base64::engine::general_purpose::STANDARD.encode(b)
    )
}

/// A broker thread serving connections one after another on a real pipe; returns the pipe name.
fn broker(tag: &str, world: &Fake) -> String {
    let name = format!("antspawn-test-client-{tag}-{}", std::process::id());
    let mut listener = Listener::new(&name, &[me()]).unwrap();
    let world = world.clone();
    std::thread::spawn(move || {
        let policy = Policy {
            images: vec!["debian-12".into()],
            profiles: vec![Profile {
                name: "small".into(),
                memory_mb: 4096,
                vcpus: 2,
                disk_gb: 20,
            }],
            sandbox_profiles: vec![],
        };
        let slots = vec![Slot {
            switch: "antspawn-sw1".into(),
            host_address: "10.77.1.1".into(),
            guest_address: "10.77.1.2".into(),
        }];
        let mut b = Broker::new(world, policy, Limits::default(), slots, 1).unwrap();
        let start = Instant::now();
        let mut conn = 0;
        while let Ok((pipe, sid)) = listener.accept() {
            conn += 1;
            let (mut r, mut w) = (&pipe, &pipe);
            serve_owned(&mut b, conn, &sid, &mut r, &mut w, || start.elapsed());
        }
    });
    name
}

#[test]
fn the_verbs_work_and_a_stop_then_start_keeps_the_same_vm() {
    let world = Fake::default();
    let name = broker("verbs", &world);
    let mut c = connect(&name).unwrap();
    let vm = c.create("debian-12", "small", &key()).unwrap();
    assert_eq!(vm.address, "10.77.1.2");
    assert_eq!(c.list().unwrap(), vec![vm.id.clone()]);
    c.stop(&vm.id).unwrap();
    c.start(&vm.id).unwrap();
    assert_eq!(
        c.list().unwrap(),
        vec![vm.id.clone()],
        "same VM after a power cycle"
    );
    c.delete(&vm.id).unwrap();
    assert!(c.list().unwrap().is_empty());
    let calls = world.0.lock().unwrap().1.clone();
    assert_eq!(
        calls,
        vec![
            format!("create {}", vm.id),
            format!("stop {}", vm.id),
            format!("start {}", vm.id),
            format!("delete {}", vm.id)
        ]
    );
}

#[test]
fn a_client_that_vanishes_loses_its_vm() {
    let world = Fake::default();
    let name = broker("vanish", &world);
    let mut c = connect(&name).unwrap();
    c.create("debian-12", "small", &key()).unwrap();
    assert_eq!(world.0.lock().unwrap().0.len(), 1);
    drop(c);
    let give_up = Instant::now() + Duration::from_secs(5);
    while !world.0.lock().unwrap().0.is_empty() {
        assert!(Instant::now() < give_up, "the VM outlived its connection");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_refusal_is_an_error_and_a_vm_that_is_not_yours_is_refused() {
    let world = Fake::default();
    let name = broker("refuse", &world);
    let mut c = connect(&name).unwrap();
    let e = c.create("windows-11", "small", &key()).unwrap_err();
    assert!(e.contains("image"), "{e}");
    let e = c.create("debian-12", "small", "not a key").unwrap_err();
    assert!(e.contains("pubkey"), "{e}");
    let e = c.delete("antspawn-999").unwrap_err();
    assert!(e.contains("no such VM") || e.contains("not yours"), "{e}");
    assert!(
        world.0.lock().unwrap().1.is_empty(),
        "a refused request reached the driver"
    );
}

#[test]
fn a_bad_pipe_name_or_a_missing_broker_is_an_error() {
    assert!(connect("a/b").is_err());
    assert!(connect("antspawn-test-nobody-here").is_err());
}
