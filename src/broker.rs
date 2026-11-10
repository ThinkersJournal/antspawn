// SPDX-License-Identifier: MIT OR Apache-2.0
//! The VM broker's core, behind a [`Driver`] (slice V2 of `docs/SANDBOX-BROKER.md`).
//!
//! This is the part that decides; the part that acts is the driver. The real Hyper-V driver (V3)
//! cannot be tested without a Hyper-V host and an account, so everything that must hold no matter
//! what the driver does is here and runs against an in-memory fake on every CI leg:
//!
//! - a request is validated ([`crate::broker_proto`]) before the driver sees anything, and the
//!   driver is given resolved values (a profile's sizes, a pool slot's switch), never request text
//!   except the one validated key;
//! - a VM belongs to the connection that created it; a disconnect, a lifetime expiry, or a failed
//!   create destroys it, and a pool slot is **held until the driver confirms the VM is gone**, so a
//!   delete that failed is retried, never forgotten;
//! - at start the broker removes any VM carrying its name prefix that it is not tracking (a
//!   crashed predecessor's), and nothing without the prefix.
//!
//! No pipe, no Hyper-V, no account: nothing here is exercised against a real peer.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::broker_proto::{Ledger, Limits, Policy, Profile, Refusal, Request, VM_PREFIX};

/// What the broker answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    /// The VM exists and is reachable at `address` (a pool slot's guest address) with the given key.
    Created {
        vm: String,
        address: String,
    },
    Done {},
    /// The Sandbox broker started one: `vm` is its name here (the owner stops it with `delete`).
    SandboxStarted {
        vm: String,
    },
    Vms {
        vms: Vec<String>,
    },
    Error {
        message: String,
    },
}

/// One pre-created, isolated network the broker may attach a VM to. Made by the elevated setup;
/// the broker never creates or changes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub switch: String,
    pub host_address: String,
    pub guest_address: String,
}

/// Everything the driver is given to build a VM: resolved by the broker, nothing free-form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSpec {
    pub id: String,
    pub image: String,
    pub profile: Profile,
    pub slot: Slot,
    pub pubkey: String,
}

/// The side that acts. Every method may fail with a reason; `delete` is idempotent (a VM that is
/// already gone is `Ok`), because "confirmed gone" is what releases a slot.
pub trait Driver {
    fn create(&mut self, spec: &VmSpec) -> Result<(), String>;
    fn start(&mut self, vm: &str) -> Result<(), String>;
    fn stop(&mut self, vm: &str) -> Result<(), String>;
    fn delete(&mut self, vm: &str) -> Result<(), String>;
    /// Every VM the host has, whoever made it.
    fn list(&self) -> Result<Vec<String>, String>;
}

fn err(message: impl Into<String>) -> Response {
    Response::Error {
        message: message.into(),
    }
}

fn refused(r: &Refusal) -> Response {
    err(r.to_string())
}

pub struct Broker<D: Driver> {
    driver: D,
    policy: Policy,
    ledger: Ledger,
    slots: Vec<Slot>,
    /// vm id -> index into `slots`, for every VM the driver may still hold (tracked or pending).
    held: BTreeMap<String, usize>,
    /// VMs whose delete failed; retried by [`Broker::tick`]. Their slots stay held.
    pending: Vec<String>,
    next_id: u64,
}

impl<D: Driver> Broker<D> {
    /// Starts the broker and removes the VMs a predecessor left (see [`Broker::sweep`]).
    ///
    /// # Errors
    /// The driver could not list, or could not delete a leftover: starting on top of VMs of unknown
    /// state would hand out a slot that is in use.
    pub fn new(
        driver: D,
        policy: Policy,
        limits: Limits,
        slots: Vec<Slot>,
        id_seed: u64,
    ) -> Result<Self, String> {
        let mut b = Broker {
            driver,
            policy,
            ledger: Ledger::new(limits),
            slots,
            held: BTreeMap::new(),
            pending: Vec::new(),
            next_id: id_seed,
        };
        b.sweep()?;
        Ok(b)
    }

    /// Deletes every VM with this broker's prefix that it is not tracking. A VM without the prefix
    /// is never touched, whatever its name looks like.
    ///
    /// # Errors
    /// The first driver failure; the rest are not attempted.
    pub fn sweep(&mut self) -> Result<usize, String> {
        let mut removed = 0;
        for vm in self.driver.list()? {
            if vm.starts_with(VM_PREFIX) && !self.held.contains_key(&vm) {
                self.driver.delete(&vm)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn driver(&self) -> &D {
        &self.driver
    }

    /// VMs whose delete failed and is being retried.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Answer one request from connection `conn`, whose pipe client the broker read as `account`.
    pub fn handle(
        &mut self,
        conn: u64,
        account: &str,
        request: &Request,
        now: Duration,
    ) -> Response {
        if let Err(r) = request.validate(&self.policy) {
            return refused(&r);
        }
        match request {
            Request::Create {
                image,
                profile,
                pubkey,
            } => self.create(conn, account, image, profile, pubkey, now),
            Request::Stop { vm } => self.act(conn, vm, |d, v| d.stop(v)),
            Request::Start { vm } => self.act(conn, vm, |d, v| d.start(v)),
            Request::Delete { vm } => self.delete(conn, vm),
            Request::List {} => Response::Vms {
                vms: self.ledger.owned_by(conn),
            },
            Request::SandboxStart { .. } => {
                err("this is the VM broker; it does not start sandboxes")
            }
        }
    }

    fn create(
        &mut self,
        conn: u64,
        account: &str,
        image: &str,
        profile: &str,
        pubkey: &str,
        now: Duration,
    ) -> Response {
        // validate() already proved both are offered
        let Some(profile) = self
            .policy
            .profiles
            .iter()
            .find(|p| p.name == profile)
            .cloned()
        else {
            return err("profile is not one this broker offers");
        };
        let Some(slot_index) = (0..self.slots.len()).find(|i| !self.held.values().any(|h| h == i))
        else {
            return err("busy: every network slot is in use");
        };
        let id = format!("{VM_PREFIX}{}", self.next_id);
        if let Err(r) = self.ledger.try_create(conn, account, &id, now) {
            return refused(&r);
        }
        self.next_id += 1;
        self.held.insert(id.clone(), slot_index);
        let slot = self.slots[slot_index].clone();
        let spec = VmSpec {
            id: id.clone(),
            image: image.to_string(),
            profile,
            slot: slot.clone(),
            pubkey: pubkey.to_string(),
        };
        if let Err(e) = self.driver.create(&spec) {
            // whatever half-exists is removed; the slot is released only once the driver confirms
            self.ledger.remove(&id);
            self.destroy(&id);
            return err(format!("create failed: {e}"));
        }
        Response::Created {
            vm: id,
            address: slot.guest_address,
        }
    }

    fn act(
        &mut self,
        conn: u64,
        vm: &str,
        f: impl FnOnce(&mut D, &str) -> Result<(), String>,
    ) -> Response {
        if let Err(r) = self.ledger.authorize(conn, vm) {
            return refused(&r);
        }
        match f(&mut self.driver, vm) {
            Ok(()) => Response::Done {},
            Err(e) => err(e),
        }
    }

    fn delete(&mut self, conn: u64, vm: &str) -> Response {
        if let Err(r) = self.ledger.authorize(conn, vm) {
            return refused(&r);
        }
        self.ledger.remove(vm);
        self.destroy(vm);
        if self.pending.iter().any(|p| p == vm) {
            // still tracked as pending: the slot stays held and the retry owns it now
            err("delete failed; it will be retried and the slot stays held until it succeeds")
        } else {
            Response::Done {}
        }
    }

    /// Delete through the driver. On success the slot is released; on failure the VM goes on the
    /// retry list and its slot stays held.
    fn destroy(&mut self, vm: &str) {
        match self.driver.delete(vm) {
            Ok(()) => {
                self.held.remove(vm);
                self.pending.retain(|p| p != vm);
            }
            Err(_) => {
                if !self.pending.iter().any(|p| p == vm) {
                    self.pending.push(vm.to_string());
                }
            }
        }
    }

    /// The pipe connection ended: destroy what it owned, nothing else.
    pub fn disconnect(&mut self, conn: u64) {
        for vm in self.ledger.disconnect(conn) {
            self.destroy(&vm);
        }
    }

    /// Periodic: expire VMs past the lifetime ceiling and retry deletes that failed.
    pub fn tick(&mut self, now: Duration) {
        for vm in self.ledger.expired(now) {
            self.destroy(&vm);
        }
        for vm in std::mem::take(&mut self.pending) {
            self.destroy(&vm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::rc::Rc;

    #[derive(Default)]
    struct World {
        vms: BTreeSet<String>,
        created: Vec<VmSpec>,
        calls: Vec<String>,
        fail_create: bool,
        fail_delete: bool,
    }

    #[derive(Clone, Default)]
    struct Fake(Rc<RefCell<World>>);

    impl Driver for Fake {
        fn create(&mut self, spec: &VmSpec) -> Result<(), String> {
            let mut w = self.0.borrow_mut();
            w.calls.push(format!("create {}", spec.id));
            // a failing create still leaves a half-made VM behind: the broker must clean it
            w.vms.insert(spec.id.clone());
            if w.fail_create {
                return Err("hyper-v said no".into());
            }
            w.created.push(spec.clone());
            Ok(())
        }
        fn start(&mut self, vm: &str) -> Result<(), String> {
            self.0.borrow_mut().calls.push(format!("start {vm}"));
            Ok(())
        }
        fn stop(&mut self, vm: &str) -> Result<(), String> {
            self.0.borrow_mut().calls.push(format!("stop {vm}"));
            Ok(())
        }
        fn delete(&mut self, vm: &str) -> Result<(), String> {
            let mut w = self.0.borrow_mut();
            w.calls.push(format!("delete {vm}"));
            if w.fail_delete {
                return Err("busy".into());
            }
            w.vms.remove(vm);
            Ok(())
        }
        fn list(&self) -> Result<Vec<String>, String> {
            Ok(self.0.borrow().vms.iter().cloned().collect())
        }
    }

    fn key() -> String {
        use base64::Engine;
        let mut b = Vec::new();
        b.extend_from_slice(&11u32.to_be_bytes());
        b.extend_from_slice(b"ssh-ed25519");
        b.extend_from_slice(&32u32.to_be_bytes());
        b.extend_from_slice(&[3u8; 32]);
        format!(
            "ssh-ed25519 {}",
            base64::engine::general_purpose::STANDARD.encode(b)
        )
    }

    fn policy() -> Policy {
        Policy {
            images: vec!["debian-12".into()],
            profiles: vec![Profile {
                name: "small".into(),
                memory_mb: 4096,
                vcpus: 2,
                disk_gb: 20,
            }],
            sandbox_profiles: vec![],
        }
    }

    fn slots(n: usize) -> Vec<Slot> {
        (0..n)
            .map(|i| Slot {
                switch: format!("antspawn-sw{i}"),
                host_address: format!("10.77.{i}.1"),
                guest_address: format!("10.77.{i}.2"),
            })
            .collect()
    }

    fn broker(world: &Fake, n_slots: usize, limits: Limits) -> Broker<Fake> {
        Broker::new(world.clone(), policy(), limits, slots(n_slots), 100).unwrap()
    }

    fn create(image: &str) -> Request {
        Request::Create {
            image: image.into(),
            profile: "small".into(),
            pubkey: key(),
        }
    }

    fn at(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn vm_of(r: &Response) -> String {
        match r {
            Response::Created { vm, .. } => vm.clone(),
            other => panic!("expected created, got {other:?}"),
        }
    }

    #[test]
    fn a_create_gives_the_driver_resolved_values_not_request_text() {
        let w = Fake::default();
        let mut b = broker(&w, 2, Limits::default());
        let r = b.handle(1, "S-A", &create("debian-12"), at(0));
        let vm = vm_of(&r);
        assert_eq!(
            r,
            Response::Created {
                vm: vm.clone(),
                address: "10.77.0.2".into()
            }
        );
        let spec = w.0.borrow().created[0].clone();
        assert_eq!(spec.profile.memory_mb, 4096);
        assert_eq!(spec.slot.switch, "antspawn-sw0");
        assert_eq!(spec.pubkey, key());
        assert!(spec.id.starts_with(VM_PREFIX));
    }

    #[test]
    fn a_refused_request_never_reaches_the_driver() {
        let w = Fake::default();
        let mut b = broker(&w, 2, Limits::default());
        for bad in [
            create(r"C:\evil.vhdx"),
            create("windows-11"),
            Request::Delete {
                vm: "my-laptop".into(),
            },
            Request::Stop {
                vm: "antspawn-1; calc".into(),
            },
        ] {
            assert!(matches!(
                b.handle(1, "S-A", &bad, at(0)),
                Response::Error { .. }
            ));
        }
        assert!(w.0.borrow().calls.is_empty(), "{:?}", w.0.borrow().calls);
    }

    #[test]
    fn running_out_of_slots_is_busy_and_leaks_nothing() {
        let w = Fake::default();
        let mut b = broker(&w, 1, Limits::default());
        let first = vm_of(&b.handle(1, "S-A", &create("debian-12"), at(0)));
        let r = b.handle(2, "S-B", &create("debian-12"), at(0));
        assert!(
            matches!(&r, Response::Error { message } if message.contains("busy")),
            "{r:?}"
        );
        assert_eq!(w.0.borrow().vms.len(), 1);
        // the slot comes back when the first VM is deleted
        assert_eq!(
            b.handle(1, "S-A", &Request::Delete { vm: first }, at(1)),
            Response::Done {}
        );
        assert!(matches!(
            b.handle(2, "S-B", &create("debian-12"), at(2)),
            Response::Created { .. }
        ));
    }

    #[test]
    fn a_failed_create_removes_what_half_exists_and_frees_the_slot_and_the_count() {
        let w = Fake::default();
        let mut b = broker(
            &w,
            1,
            Limits {
                max_total: 1,
                ..Limits::default()
            },
        );
        w.0.borrow_mut().fail_create = true;
        let r = b.handle(1, "S-A", &create("debian-12"), at(0));
        assert!(matches!(&r, Response::Error { message } if message.contains("create failed")));
        assert!(
            w.0.borrow().vms.is_empty(),
            "the half-made VM was left behind"
        );
        w.0.borrow_mut().fail_create = false;
        assert!(
            matches!(
                b.handle(1, "S-A", &create("debian-12"), at(1)),
                Response::Created { .. }
            ),
            "slot and cap were not given back"
        );
    }

    #[test]
    fn another_connection_cannot_act_on_or_see_a_vm_even_for_the_same_account() {
        let w = Fake::default();
        let mut b = broker(&w, 2, Limits::default());
        let vm = vm_of(&b.handle(1, "S-A", &create("debian-12"), at(0)));
        for req in [
            Request::Stop { vm: vm.clone() },
            Request::Start { vm: vm.clone() },
            Request::Delete { vm: vm.clone() },
        ] {
            let r = b.handle(2, "S-A", &req, at(1));
            assert!(
                matches!(&r, Response::Error { message } if message.contains("not yours")),
                "{r:?}"
            );
        }
        assert_eq!(
            b.handle(2, "S-A", &Request::List {}, at(1)),
            Response::Vms { vms: vec![] }
        );
        assert_eq!(
            b.handle(1, "S-A", &Request::List {}, at(1)),
            Response::Vms {
                vms: vec![vm.clone()]
            }
        );
        let calls = w.0.borrow().calls.clone();
        assert_eq!(
            calls,
            vec![format!("create {vm}")],
            "the other connection moved nothing"
        );
    }

    #[test]
    fn a_disconnect_destroys_that_connections_vms_and_only_those() {
        let w = Fake::default();
        let mut b = broker(&w, 3, Limits::default());
        let mine = vm_of(&b.handle(1, "S-A", &create("debian-12"), at(0)));
        let theirs = vm_of(&b.handle(2, "S-B", &create("debian-12"), at(0)));
        b.disconnect(1);
        assert!(!w.0.borrow().vms.contains(&mine));
        assert!(w.0.borrow().vms.contains(&theirs));
    }

    #[test]
    fn a_delete_that_fails_keeps_the_slot_and_is_retried_until_it_works() {
        let w = Fake::default();
        let mut b = broker(&w, 1, Limits::default());
        let vm = vm_of(&b.handle(1, "S-A", &create("debian-12"), at(0)));
        w.0.borrow_mut().fail_delete = true;
        b.disconnect(1);
        assert_eq!(b.pending(), 1);
        // the VM still exists, so its slot must not be handed out
        let r = b.handle(2, "S-B", &create("debian-12"), at(1));
        assert!(
            matches!(&r, Response::Error { message } if message.contains("busy")),
            "{r:?}"
        );
        b.tick(at(2));
        assert_eq!(b.pending(), 1, "still failing, still pending");
        w.0.borrow_mut().fail_delete = false;
        b.tick(at(3));
        assert_eq!(b.pending(), 0);
        assert!(!w.0.borrow().vms.contains(&vm));
        assert!(matches!(
            b.handle(2, "S-B", &create("debian-12"), at(4)),
            Response::Created { .. }
        ));
    }

    #[test]
    fn a_vm_past_its_lifetime_is_destroyed_by_tick() {
        let w = Fake::default();
        let mut b = broker(
            &w,
            2,
            Limits {
                max_lifetime: at(100),
                ..Limits::default()
            },
        );
        let vm = vm_of(&b.handle(1, "S-A", &create("debian-12"), at(0)));
        b.tick(at(99));
        assert!(w.0.borrow().vms.contains(&vm));
        b.tick(at(100));
        assert!(!w.0.borrow().vms.contains(&vm));
    }

    #[test]
    fn at_start_the_broker_removes_its_own_leftovers_and_nothing_else() {
        let w = Fake::default();
        {
            let mut world = w.0.borrow_mut();
            world.vms.insert("antspawn-7".into());
            world.vms.insert("antspawn-v0-vm".into());
            world.vms.insert("my-laptop-vm".into());
            world.vms.insert("antspawnish".into());
        }
        let b = broker(&w, 1, Limits::default());
        let left: Vec<String> = b.driver().0.borrow().vms.iter().cloned().collect();
        assert_eq!(left, vec!["antspawnish", "my-laptop-vm"]);
    }

    #[test]
    fn a_broker_that_cannot_clean_its_leftovers_does_not_start() {
        let w = Fake::default();
        w.0.borrow_mut().vms.insert("antspawn-7".into());
        w.0.borrow_mut().fail_delete = true;
        assert!(Broker::new(w.clone(), policy(), Limits::default(), slots(1), 1).is_err());
    }

    #[test]
    fn the_vm_broker_does_not_start_sandboxes() {
        let w = Fake::default();
        let mut b = broker(&w, 1, Limits::default());
        let r = b.handle(
            1,
            "S-A",
            &Request::SandboxStart {
                profile: "default".into(),
            },
            at(0),
        );
        assert!(matches!(r, Response::Error { .. }));
        assert!(w.0.borrow().calls.is_empty());
    }

    #[test]
    fn stop_and_start_reach_the_driver_for_the_owner() {
        let w = Fake::default();
        let mut b = broker(&w, 1, Limits::default());
        let vm = vm_of(&b.handle(1, "S-A", &create("debian-12"), at(0)));
        assert_eq!(
            b.handle(1, "S-A", &Request::Stop { vm: vm.clone() }, at(1)),
            Response::Done {}
        );
        assert_eq!(
            b.handle(1, "S-A", &Request::Start { vm: vm.clone() }, at(2)),
            Response::Done {}
        );
        let calls = w.0.borrow().calls.clone();
        assert_eq!(calls[1..], [format!("stop {vm}"), format!("start {vm}")]);
    }
}
