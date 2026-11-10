// SPDX-License-Identifier: MIT OR Apache-2.0
//! One broker connection, over any byte stream (slice V3 of `docs/SANDBOX-BROKER.md`): frames in,
//! [`Broker::handle`], frames out, and the VMs the connection made destroyed when it ends, however
//! it ends.
//!
//! No pipe and no account here: the caller (the broker binary) accepts the pipe,
//! reads the client's SID from the OS, and passes it in as `account`. What is shown by the tests is
//! the loop's behaviour over an in-memory stream; the real pipe is not exercised.

use std::io::{Read, Write};
use std::time::Duration;

use crate::broker::{Broker, Driver, Response};
use crate::broker_proto::Request;
use crate::session_proto::{ProtoError, read_frame, write_frame};

/// Why [`serve_connection`] returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The peer went away between frames.
    Disconnected,
    /// A frame left the stream out of step (bad length, truncated) or the pipe failed.
    Broken(String),
}

/// Serve one connected peer until it disconnects or breaks the framing, then call `finish`.
///
/// `handle` answers one valid request; a caller sharing one broker between connections locks it
/// inside `handle`, so the lock is held per request and never across a blocking read. `finish` runs on
/// EVERY exit path (a peer that vanished, broke the framing, or could not be answered) and is where
/// the connection's VMs are destroyed.
///
/// A frame that is intact but not a valid request (bad JSON, unknown op, extra field) is answered
/// with an error and the loop goes on: the stream is still in step. A bad length or a truncated
/// frame ends the connection.
pub fn serve_connection<R: Read, W: Write>(
    r: &mut R,
    w: &mut W,
    mut handle: impl FnMut(&Request) -> Response,
    finish: impl FnOnce(),
) -> Ended {
    let ended = loop {
        let request: Request = match read_frame(r) {
            Ok(Some(request)) => request,
            Ok(None) => break Ended::Disconnected,
            Err(e @ (ProtoError::Malformed(_) | ProtoError::Invalid(_))) => {
                let answer = Response::Error {
                    message: e.to_string(),
                };
                if let Err(e) = write_frame(w, &answer) {
                    break Ended::Broken(e.to_string());
                }
                continue;
            }
            Err(e) => break Ended::Broken(e.to_string()),
        };
        let answer = handle(&request);
        if let Err(e) = write_frame(w, &answer) {
            break Ended::Broken(e.to_string());
        }
    };
    finish();
    ended
}

/// [`serve_connection`] for a broker this connection owns exclusively (tests, a single-client run).
/// `now` is the broker's own clock, read per request.
pub fn serve_owned<D: Driver, R: Read, W: Write>(
    broker: &mut Broker<D>,
    conn: u64,
    account: &str,
    r: &mut R,
    w: &mut W,
    mut now: impl FnMut() -> Duration,
) -> Ended {
    let broker = std::cell::RefCell::new(broker);
    serve_connection(
        r,
        w,
        |request| broker.borrow_mut().handle(conn, account, request, now()),
        || broker.borrow_mut().disconnect(conn),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::Slot;
    use crate::broker::VmSpec;
    use crate::broker_proto::{Limits, Policy, Profile};
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::io::Cursor;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Fake(Rc<RefCell<BTreeSet<String>>>);

    impl Driver for Fake {
        fn create(&mut self, spec: &VmSpec) -> Result<(), String> {
            self.0.borrow_mut().insert(spec.id.clone());
            Ok(())
        }
        fn start(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn stop(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn delete(&mut self, vm: &str) -> Result<(), String> {
            self.0.borrow_mut().remove(vm);
            Ok(())
        }
        fn list(&self) -> Result<Vec<String>, String> {
            Ok(self.0.borrow().iter().cloned().collect())
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

    fn broker(world: &Fake) -> Broker<Fake> {
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
        Broker::new(world.clone(), policy, Limits::default(), slots, 1).unwrap()
    }

    fn frames(reqs: &[Request]) -> Vec<u8> {
        let mut v = Vec::new();
        for r in reqs {
            write_frame(&mut v, r).unwrap();
        }
        v
    }

    fn answers(out: &[u8]) -> Vec<Response> {
        let mut c = Cursor::new(out);
        let mut v = Vec::new();
        while let Some(r) = read_frame::<_, Response>(&mut c).unwrap() {
            v.push(r);
        }
        v
    }

    fn create() -> Request {
        Request::Create {
            image: "debian-12".into(),
            profile: "small".into(),
            pubkey: key(),
        }
    }

    #[test]
    fn a_peer_that_leaves_without_deleting_has_its_vm_destroyed() {
        let world = Fake::default();
        let mut b = broker(&world);
        let mut input = Cursor::new(frames(&[create()]));
        let mut out = Vec::new();
        let ended = serve_owned(
            &mut b,
            1,
            "S-1-5-21-1-2-3-1001",
            &mut input,
            &mut out,
            || Duration::ZERO,
        );
        assert_eq!(ended, Ended::Disconnected);
        assert!(matches!(answers(&out)[0], Response::Created { .. }));
        assert!(
            world.0.borrow().is_empty(),
            "the VM outlived its connection"
        );
    }

    #[test]
    fn a_bad_frame_is_answered_and_the_loop_goes_on() {
        let world = Fake::default();
        let mut b = broker(&world);
        let bad = br#"{"op":"list","extra":1}"#;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(bad.len() as u32).to_le_bytes());
        bytes.extend_from_slice(bad);
        bytes.extend(frames(&[Request::List {}]));
        let mut out = Vec::new();
        let ended = serve_owned(
            &mut b,
            1,
            "S-1-5-21-1-2-3-1001",
            &mut Cursor::new(bytes),
            &mut out,
            || Duration::ZERO,
        );
        assert_eq!(ended, Ended::Disconnected);
        let a = answers(&out);
        assert!(matches!(a[0], Response::Error { .. }));
        assert!(matches!(a[1], Response::Vms { .. }));
    }

    #[test]
    fn a_truncated_frame_ends_the_connection_and_still_cleans_up() {
        let world = Fake::default();
        let mut b = broker(&world);
        let mut bytes = frames(&[create()]);
        bytes.extend_from_slice(&100u32.to_le_bytes());
        bytes.extend_from_slice(b"{\"op\"");
        let mut out = Vec::new();
        let ended = serve_owned(
            &mut b,
            1,
            "S-1-5-21-1-2-3-1001",
            &mut Cursor::new(bytes),
            &mut out,
            || Duration::ZERO,
        );
        assert!(matches!(ended, Ended::Broken(_)), "{ended:?}");
        assert!(world.0.borrow().is_empty());
    }

    #[test]
    fn an_oversized_length_ends_it_before_anything_is_read() {
        let world = Fake::default();
        let mut b = broker(&world);
        let bytes = u32::MAX.to_le_bytes().to_vec();
        let mut out = Vec::new();
        let ended = serve_owned(
            &mut b,
            1,
            "S-1-5-21-1-2-3-1001",
            &mut Cursor::new(bytes),
            &mut out,
            || Duration::ZERO,
        );
        assert!(matches!(ended, Ended::Broken(_)));
    }
}
