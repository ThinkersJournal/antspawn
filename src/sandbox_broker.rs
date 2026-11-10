// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Windows Sandbox broker's core, behind a [`SandboxDriver`] (slice SB1/SB2 of
//! `docs/SANDBOX-BROKER.md`, Track 2).
//!
//! The same shape and the same guarantees as [`crate::broker`], for a different resource:
//!
//! - a request is validated ([`crate::broker_proto`]) before the driver sees anything, and the driver
//!   is given a profile name that this broker's own policy offers, never request text beyond it;
//! - ONE sandbox at a time (Windows Sandbox allows a single instance, `CO_E_APPSINGLEUSE`), and that
//!   one is **held until the driver confirms it is gone**: a stop that failed is retried by
//!   [`SandboxBroker::tick`], and nothing else starts meanwhile;
//! - a sandbox belongs to the connection that started it: a disconnect, a lifetime expiry or a failed
//!   start destroys it, and only the owner may stop it.
//!
//! Unlike the VM broker there is **no sweep at start**. Sandboxes have no name prefix of ours, and
//! `wsb list` shows every sandbox of every owner, including the user's own work; the broker cannot tell
//! a predecessor's from someone else's and so never stops one it did not start. A crashed broker's
//! sandbox lingers until it is stopped by hand (or ends itself). That is a known gap, not a feature.
//!
//! No `wsb`, no pipe, no account: nothing here runs against a real Windows Sandbox.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::broker::Response;
use crate::broker_proto::{Ledger, Limits, Policy, Refusal, Request, VM_PREFIX};

/// The side that acts. `stop` is idempotent (a sandbox that is already gone is `Ok`), because
/// "confirmed gone" is what releases the single slot.
pub trait SandboxDriver {
    /// Start a sandbox from the fixed template named `profile`; returns the id `wsb` gave it.
    ///
    /// # Errors
    /// Why it could not start.
    fn start(&mut self, profile: &str) -> Result<String, String>;
    /// # Errors
    /// Why it could not be stopped.
    fn stop(&mut self, sandbox: &str) -> Result<(), String>;
}

fn err(message: impl Into<String>) -> Response {
    Response::Error {
        message: message.into(),
    }
}

pub struct SandboxBroker<D: SandboxDriver> {
    driver: D,
    policy: Policy,
    ledger: Ledger,
    /// our name -> the id `wsb` gave it, for every sandbox the driver may still hold.
    held: BTreeMap<String, String>,
    /// names whose stop failed; retried by [`SandboxBroker::tick`]. Still in `held`.
    pending: Vec<String>,
    next_id: u64,
}

impl<D: SandboxDriver> SandboxBroker<D> {
    /// `limits.max_total` is forced to 1 and `max_per_account` to at most 1: one sandbox at a time.
    pub fn new(driver: D, policy: Policy, mut limits: Limits, id_seed: u64) -> Self {
        limits.max_total = 1;
        limits.max_per_account = limits.max_per_account.min(1);
        Self {
            driver,
            policy,
            ledger: Ledger::new(limits),
            held: BTreeMap::new(),
            pending: Vec::new(),
            next_id: id_seed,
        }
    }

    pub fn driver(&self) -> &D {
        &self.driver
    }

    /// Sandboxes whose stop failed and is being retried.
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
            return err(r.to_string());
        }
        match request {
            Request::SandboxStart { profile } => self.start(conn, account, profile, now),
            Request::Delete { vm } | Request::Stop { vm } => self.stop(conn, vm),
            Request::List {} => Response::Vms {
                vms: self.ledger.owned_by(conn),
            },
            Request::Create { .. } | Request::Start { .. } => {
                err("this is the Sandbox broker; it does not make VMs")
            }
        }
    }

    fn start(&mut self, conn: u64, account: &str, profile: &str, now: Duration) -> Response {
        if !self.held.is_empty() {
            return err("busy: a sandbox is already running (Windows Sandbox allows one)");
        }
        let name = format!("{VM_PREFIX}sbx-{}", self.next_id);
        if let Err(r) = self.ledger.try_create(conn, account, &name, now) {
            return err(r.to_string());
        }
        self.next_id += 1;
        match self.driver.start(profile) {
            Ok(wsb_id) => {
                self.held.insert(name.clone(), wsb_id);
                Response::SandboxStarted { vm: name }
            }
            Err(e) => {
                // Nothing is known to be running, so nothing is held; the failure is the answer.
                self.ledger.remove(&name);
                err(format!("start failed: {e}"))
            }
        }
    }

    fn stop(&mut self, conn: u64, vm: &str) -> Response {
        if let Err(r) = self.ledger.authorize(conn, vm) {
            return err(refusal(&r));
        }
        self.ledger.remove(vm);
        self.destroy(vm);
        if self.pending.iter().any(|p| p == vm) {
            err("stop failed; it will be retried and no other sandbox starts until it succeeds")
        } else {
            Response::Done {}
        }
    }

    fn destroy(&mut self, name: &str) {
        let Some(wsb_id) = self.held.get(name).cloned() else {
            return;
        };
        match self.driver.stop(&wsb_id) {
            Ok(()) => {
                self.held.remove(name);
                self.pending.retain(|p| p != name);
            }
            Err(_) => {
                if !self.pending.iter().any(|p| p == name) {
                    self.pending.push(name.to_string());
                }
            }
        }
    }

    /// The pipe connection ended: stop what it started, nothing else.
    pub fn disconnect(&mut self, conn: u64) {
        for name in self.ledger.disconnect(conn) {
            self.destroy(&name);
        }
    }

    /// Periodic: expire sandboxes past the lifetime ceiling and retry stops that failed.
    pub fn tick(&mut self, now: Duration) {
        for name in self.ledger.expired(now) {
            self.destroy(&name);
        }
        for name in std::mem::take(&mut self.pending) {
            self.destroy(&name);
        }
    }
}

fn refusal(r: &Refusal) -> String {
    r.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct World {
        running: Vec<String>,
        calls: Vec<String>,
        fail_start: bool,
        fail_stop: bool,
        n: u32,
    }

    #[derive(Clone, Default)]
    struct Fake(Rc<RefCell<World>>);

    impl SandboxDriver for Fake {
        fn start(&mut self, profile: &str) -> Result<String, String> {
            let mut w = self.0.borrow_mut();
            w.calls.push(format!("start {profile}"));
            if w.fail_start {
                return Err("CO_E_APPSINGLEUSE".into());
            }
            w.n += 1;
            let id = format!("wsb-{}", w.n);
            w.running.push(id.clone());
            Ok(id)
        }
        fn stop(&mut self, id: &str) -> Result<(), String> {
            let mut w = self.0.borrow_mut();
            w.calls.push(format!("stop {id}"));
            if w.fail_stop {
                return Err("busy".into());
            }
            w.running.retain(|r| r != id);
            Ok(())
        }
    }

    fn policy() -> Policy {
        Policy {
            images: vec![],
            profiles: vec![],
            sandbox_profiles: vec!["default".into()],
        }
    }

    fn broker(w: &Fake) -> SandboxBroker<Fake> {
        SandboxBroker::new(w.clone(), policy(), Limits::default(), 1)
    }

    fn start() -> Request {
        Request::SandboxStart {
            profile: "default".into(),
        }
    }

    const T0: Duration = Duration::ZERO;

    fn started(r: Response) -> String {
        match r {
            Response::SandboxStarted { vm } => vm,
            other => panic!("expected a start, got {other:?}"),
        }
    }

    #[test]
    fn a_start_gives_the_driver_only_the_offered_profile_name() {
        let w = Fake::default();
        let mut b = broker(&w);
        started(b.handle(1, "S-1-5-21-1", &start(), T0));
        assert_eq!(w.0.borrow().calls, vec!["start default".to_string()]);
    }

    #[test]
    fn a_refused_request_never_reaches_the_driver() {
        let w = Fake::default();
        let mut b = broker(&w);
        for bad in [
            Request::SandboxStart {
                profile: "C:\\Users\\x".into(),
            },
            Request::SandboxStart {
                profile: "other".into(),
            },
            Request::SandboxStart {
                profile: "default\n".into(),
            },
            Request::Delete {
                vm: "my-laptop".into(),
            },
        ] {
            let r = b.handle(1, "S-1-5-21-1", &bad, T0);
            assert!(matches!(r, Response::Error { .. }), "{bad:?} -> {r:?}");
        }
        assert!(w.0.borrow().calls.is_empty());
    }

    #[test]
    fn one_sandbox_at_a_time_even_for_another_account() {
        let w = Fake::default();
        let mut b = broker(&w);
        started(b.handle(1, "S-1-5-21-1", &start(), T0));
        let r = b.handle(2, "S-1-5-21-2", &start(), T0);
        assert!(
            matches!(&r, Response::Error { message } if message.contains("busy")),
            "{r:?}"
        );
        assert_eq!(w.0.borrow().running.len(), 1);
    }

    #[test]
    fn a_disconnect_stops_the_sandbox_and_frees_the_slot() {
        let w = Fake::default();
        let mut b = broker(&w);
        started(b.handle(1, "S-1-5-21-1", &start(), T0));
        b.disconnect(1);
        assert!(w.0.borrow().running.is_empty());
        started(b.handle(2, "S-1-5-21-2", &start(), T0));
    }

    #[test]
    fn only_the_owner_may_stop_it() {
        let w = Fake::default();
        let mut b = broker(&w);
        let vm = started(b.handle(1, "S-1-5-21-1", &start(), T0));
        let r = b.handle(2, "S-1-5-21-1", &Request::Delete { vm: vm.clone() }, T0);
        assert!(matches!(r, Response::Error { .. }), "{r:?}");
        assert_eq!(w.0.borrow().running.len(), 1, "a stranger stopped it");
        assert_eq!(
            b.handle(1, "S-1-5-21-1", &Request::Delete { vm }, T0),
            Response::Done {}
        );
        assert!(w.0.borrow().running.is_empty());
    }

    #[test]
    fn a_failed_start_holds_nothing() {
        let w = Fake::default();
        w.0.borrow_mut().fail_start = true;
        let mut b = broker(&w);
        let r = b.handle(1, "S-1-5-21-1", &start(), T0);
        assert!(matches!(&r, Response::Error { message } if message.contains("start failed")));
        w.0.borrow_mut().fail_start = false;
        started(b.handle(1, "S-1-5-21-1", &start(), T0));
    }

    #[test]
    fn a_stop_that_fails_keeps_the_slot_held_and_is_retried() {
        let w = Fake::default();
        let mut b = broker(&w);
        let vm = started(b.handle(1, "S-1-5-21-1", &start(), T0));
        w.0.borrow_mut().fail_stop = true;
        let r = b.handle(1, "S-1-5-21-1", &Request::Delete { vm }, T0);
        assert!(matches!(&r, Response::Error { message } if message.contains("retried")));
        assert_eq!(b.pending(), 1);
        let busy = b.handle(2, "S-1-5-21-2", &start(), T0);
        assert!(matches!(&busy, Response::Error { message } if message.contains("busy")));
        w.0.borrow_mut().fail_stop = false;
        b.tick(T0);
        assert_eq!(b.pending(), 0);
        assert!(w.0.borrow().running.is_empty());
        started(b.handle(2, "S-1-5-21-2", &start(), T0));
    }

    #[test]
    fn a_sandbox_past_its_lifetime_is_stopped_by_tick() {
        let w = Fake::default();
        let mut b = broker(&w);
        started(b.handle(1, "S-1-5-21-1", &start(), T0));
        b.tick(Limits::default().max_lifetime + Duration::from_secs(1));
        assert!(w.0.borrow().running.is_empty());
    }

    #[test]
    fn vm_verbs_are_refused_and_list_shows_only_your_own() {
        let w = Fake::default();
        let mut b = broker(&w);
        for r in [
            Request::Create {
                image: "x".into(),
                profile: "y".into(),
                pubkey: "z".into(),
            },
            Request::Start {
                vm: "antspawn-1".into(),
            },
        ] {
            assert!(matches!(
                b.handle(1, "S-1-5-21-1", &r, T0),
                Response::Error { .. }
            ));
        }
        let vm = started(b.handle(1, "S-1-5-21-1", &start(), T0));
        assert_eq!(
            b.handle(1, "S-1-5-21-1", &Request::List {}, T0),
            Response::Vms { vms: vec![vm] }
        );
        assert_eq!(
            b.handle(2, "S-1-5-21-1", &Request::List {}, T0),
            Response::Vms { vms: vec![] }
        );
    }
}
