// SPDX-License-Identifier: MIT OR Apache-2.0
//! The client side of the VM broker (slice V4 of `docs/SANDBOX-BROKER.md`): one connection, the five
//! verbs a test needs (`create`, `start`, `stop`, `delete`, `list`), over any byte stream.
//!
//! The connection IS the VM's owner: the broker destroys what a connection made when it ends, so a
//! client that wants a VM to survive a stop and a start keeps the connection open across them, and one
//! that crashes loses its VM. Dropping a [`Client`] closes the stream and that is the cleanup.
//!
//! Every answer that is an error from the broker is an `Err(message)`; a reply of the wrong kind for
//! the verb is an error too, never a guess. Shown by the tests: the verbs over a real pipe against a
//! fake driver, and the cleanup when the client vanishes. NOT shown: anything against real Hyper-V (so
//! not the PersistAnt power-cut itself, which is the first real user of this).

use std::io::{Read, Write};

use crate::broker::Response;
use crate::broker_proto::Request;
use crate::session_proto::{read_frame, write_frame};

/// A VM the broker made: its name, and the address its guest answers on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vm {
    pub id: String,
    pub address: String,
}

pub struct Client<S: Read + Write> {
    io: S,
}

impl<S: Read + Write> Client<S> {
    pub fn new(io: S) -> Self {
        Self { io }
    }

    fn call(&mut self, request: &Request) -> Result<Response, String> {
        write_frame(&mut self.io, request).map_err(|e| format!("sending: {e}"))?;
        match read_frame::<_, Response>(&mut self.io) {
            Ok(Some(Response::Error { message })) => Err(message),
            Ok(Some(other)) => Ok(other),
            Ok(None) => Err("the broker closed the connection".into()),
            Err(e) => Err(format!("reading the answer: {e}")),
        }
    }

    /// # Errors
    /// The broker refused (its message), or the answer was not a `Created`.
    pub fn create(&mut self, image: &str, profile: &str, pubkey: &str) -> Result<Vm, String> {
        let r = self.call(&Request::Create {
            image: image.into(),
            profile: profile.into(),
            pubkey: pubkey.into(),
        })?;
        match r {
            Response::Created { vm, address } => Ok(Vm { id: vm, address }),
            _ => Err("the broker answered a create with something else".into()),
        }
    }

    fn done(&mut self, request: &Request) -> Result<(), String> {
        match self.call(request)? {
            Response::Done {} => Ok(()),
            _ => Err("the broker answered with something other than done".into()),
        }
    }

    /// # Errors
    /// The broker refused, or the answer was not `done`.
    pub fn start(&mut self, vm: &str) -> Result<(), String> {
        self.done(&Request::Start { vm: vm.into() })
    }

    /// # Errors
    /// The broker refused, or the answer was not `done`.
    pub fn stop(&mut self, vm: &str) -> Result<(), String> {
        self.done(&Request::Stop { vm: vm.into() })
    }

    /// # Errors
    /// The broker refused (a delete that failed is retried by the broker and reported here), or the
    /// answer was not `done`.
    pub fn delete(&mut self, vm: &str) -> Result<(), String> {
        self.done(&Request::Delete { vm: vm.into() })
    }

    /// This connection's own VMs.
    ///
    /// # Errors
    /// The broker refused, or the answer was not a list.
    pub fn list(&mut self) -> Result<Vec<String>, String> {
        match self.call(&Request::List {})? {
            Response::Vms { vms } => Ok(vms),
            _ => Err("the broker answered a list with something else".into()),
        }
    }
}

/// Wait until something accepts a TCP connection at `addr`, polling until `timeout`.
///
/// # Errors
/// Nothing answered in time (the last connect error is in the message).
pub fn wait_port(addr: std::net::SocketAddr, timeout: std::time::Duration) -> Result<(), String> {
    use std::time::{Duration, Instant};
    let give_up = Instant::now() + timeout;
    loop {
        let last = match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
            Ok(_) => return Ok(()),
            Err(e) => e,
        };
        if Instant::now() >= give_up {
            return Err(format!("nothing answered at {addr} in time: {last}"));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Wait for a broker-made guest's SSH port (22) at `address`, the `address` a `create` returned.
/// `create` does not start the VM and the driver does not wait for the guest, so the caller starts it
/// and waits here. Only an address inside the pool range (10.77.0.0/16) is accepted: this is not a
/// general port scanner.
///
/// # Errors
/// `address` is not an IPv4 address in 10.77.0.0/16, or nothing answered in time.
pub fn wait_ssh(address: &str, timeout: std::time::Duration) -> Result<(), String> {
    let ip: std::net::Ipv4Addr = address
        .parse()
        .map_err(|_| "the address is not an IPv4 address".to_string())?;
    if ip.octets()[..2] != [10, 77] {
        return Err("the address is outside 10.77.0.0/16".into());
    }
    wait_port(std::net::SocketAddr::from((ip, 22)), timeout)
}

/// Connect to the broker's pipe, waiting briefly while every instance is busy.
///
/// # Errors
/// A bad pipe name, the broker is not there, or access was refused (the account is not on the
/// broker's list).
#[cfg(windows)]
pub fn connect(name: &str) -> Result<Client<std::fs::File>, String> {
    use std::time::{Duration, Instant};
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("pipe name must be 1..=64 of a-z A-Z 0-9 - _".into());
    }
    let path = format!(r"\\.\pipe\{name}");
    let give_up = Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(f) => return Ok(Client::new(f)),
            // ERROR_PIPE_BUSY: every instance has a client; the broker makes the next one right away.
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < give_up => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("cannot reach the broker: {e}")),
        }
    }
}

#[cfg(not(windows))]
pub fn connect(_: &str) -> Result<Client<std::fs::File>, String> {
    Err("the broker pipe is implemented for Windows only".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_listening_port_is_found_and_a_closed_one_times_out() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = l.local_addr().unwrap();
        wait_port(open, Duration::from_secs(2)).unwrap();
        drop(l);
        let started = std::time::Instant::now();
        let e = wait_port(open, Duration::from_millis(600)).unwrap_err();
        assert!(e.contains("in time"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn wait_ssh_refuses_anything_outside_the_pool_range() {
        for bad in [
            "192.168.1.5",
            "10.78.0.1",
            "127.0.0.1",
            "10.77.1.2 ",
            "nope",
            "",
        ] {
            let e = wait_ssh(bad, Duration::from_millis(10)).unwrap_err();
            assert!(e.contains("address"), "{bad}: {e}");
        }
    }
}
