// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host end of the runner hand-off over the network (board 322, CireSnave 2026-10-30, "A:
//! one-shot listener in the sandbox"): find the guest's address with `wsb ip`, connect to the
//! `spawn-jit-shim --listen` there, send the JIT config once and close.
//!
//! The value is written to the socket and nowhere else: not an argument, a variable, a file or a
//! message. Errors name the address and the I/O kind, never the config. **Not run against a VM** until
//! board 321 is settled; the `wsb ip` output format is unmeasured, so [`parse_wsb_ip`] accepts only
//! an unambiguous single IPv4 address and refuses anything else.

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};

use super::github::Secret;
use crate::launchers::windows_sandbox::wsb_exe;

/// The one IPv4 address in `wsb ip` output; `None` if there is none or more than one distinct one.
pub fn parse_wsb_ip(stdout: &str) -> Option<Ipv4Addr> {
    let mut found: Option<Ipv4Addr> = None;
    for token in stdout.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        if let Ok(ip) = token.parse::<Ipv4Addr>() {
            match found {
                Some(prev) if prev != ip => return None,
                _ => found = Some(ip),
            }
        }
    }
    found.filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
}

/// Ask `wsb ip --id <id>` for the guest address.
pub fn guest_ip(sandbox_id: &str) -> Result<Ipv4Addr, String> {
    let out = Command::new(wsb_exe())
        .args(["ip", "--id", sandbox_id])
        .output()
        .map_err(|e| format!("could not run wsb ip: {}", e.kind()))?;
    if !out.status.success() {
        return Err(format!("wsb ip failed ({})", out.status));
    }
    parse_wsb_ip(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "wsb ip did not give exactly one IPv4 address".to_string())
}

/// Connect to `addr` (retrying a refused connection until `window` ends, since the guest listener
/// starts after boot), write `jit` once and close. A connection that was made is never retried: the
/// listener takes exactly one, so a second attempt could only be refused or reach a different peer.
pub fn send_once(addr: SocketAddr, jit: &Secret, window: Duration) -> Result<(), String> {
    let deadline = Instant::now() + window;
    let mut stream = loop {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(stream) => break stream,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => return Err(format!("could not connect to {addr}: {}", e.kind())),
        }
    };
    stream
        .write_all(jit.expose().as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| format!("could not send to {addr}: {}", e.kind()))?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    Ok(())
}

/// [`guest_ip`] then [`send_once`] on `port`.
pub fn deliver_to_sandbox(
    sandbox_id: &str,
    port: u16,
    jit: &Secret,
    window: Duration,
) -> Result<(), String> {
    let ip = guest_ip(sandbox_id)?;
    send_once(SocketAddr::new(IpAddr::V4(ip), port), jit, window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    #[test]
    fn one_address_in_any_wording_is_taken() {
        for out in [
            "172.28.80.5",
            "IP: 172.28.80.5\r\n",
            "{\"ip\":\"172.28.80.5\"}",
            "172.28.80.5\n172.28.80.5",
        ] {
            assert_eq!(
                parse_wsb_ip(out),
                Some(Ipv4Addr::new(172, 28, 80, 5)),
                "{out:?}"
            );
        }
    }

    #[test]
    fn control_nothing_ambiguous_or_unusable_is_taken() {
        for out in [
            "",
            "no address",
            "1.2.3",
            "172.28.80.5 172.28.80.6",
            "0.0.0.0",
            "127.0.0.1",
            "999.1.1.1",
        ] {
            assert_eq!(parse_wsb_ip(out), None, "{out:?}");
        }
    }

    fn one_shot_receiver() -> (SocketAddr, std::thread::JoinHandle<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).unwrap();
            got
        });
        (addr, h)
    }

    #[test]
    fn the_config_arrives_once_and_exactly() {
        let (addr, h) = one_shot_receiver();
        send_once(
            addr,
            &Secret::new("c2VjcmV0Cg==".into()),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(h.join().unwrap(), b"c2VjcmV0Cg==");
    }

    #[test]
    fn a_listener_that_starts_late_is_still_reached() {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            let l = TcpListener::bind(addr).unwrap();
            let (mut s, _) = l.accept().unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).unwrap();
            got
        });
        send_once(addr, &Secret::new("YWJj".into()), Duration::from_secs(5)).unwrap();
        assert_eq!(h.join().unwrap(), b"YWJj");
    }

    #[test]
    fn control_no_listener_is_an_error_that_names_no_secret() {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);
        let err = send_once(
            addr,
            &Secret::new("c2VjcmV0Cg==".into()),
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(err.contains("could not connect"), "{err}");
        assert!(!err.contains("c2VjcmV0"), "{err}");
    }
}
