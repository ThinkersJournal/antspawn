// SPDX-License-Identifier: MIT OR Apache-2.0
//! The egress filtering proxy's core (`docs/RUNNER-NETWORK.md`, ROADMAP row 38): an HTTP `CONNECT` proxy that
//! allows named hosts on port 443 and nothing else. It never looks inside the tunnel (no TLS interception),
//! resolves the name itself, refuses a name that resolves to a loopback, private or link-local address (an
//! allowed name must not reach the host's own services), and fails closed: any request it cannot read in full
//! is refused. The decision is a pure function ([`decide`]) so each rule has a test with a known-bad control;
//! [`serve`] only adds sockets.
//!
//! Not yet wired to a launcher, a switch or a firewall rule (rows 39 and 40): this is the proxy process's logic.

use std::fmt;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The only port a tunnel may reach.
pub const ALLOWED_PORT: u16 = 443;
/// A request head longer than this is refused (a `CONNECT` line is far shorter).
pub const MAX_HEAD: usize = 8 * 1024;
/// A ClientHello record longer than this is refused when the SNI check is on.
pub const MAX_HELLO: usize = 16 * 1024;

/// One allowlist entry: an exact host name, or `*.suffix` (any host under `suffix`, not `suffix` itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    Exact(String),
    Under(String),
}

impl Pattern {
    /// Parse `host` or `*.suffix`. A bare `*`, an IP literal, a port, a path or any other wildcard shape is refused.
    pub fn parse(text: &str) -> Result<Pattern, String> {
        let t = text.trim().to_ascii_lowercase();
        if let Some(rest) = t.strip_prefix("*.") {
            if !valid_name(rest) || !rest.contains('.') {
                return Err(format!("`{text}`: a wildcard needs a full domain under it"));
            }
            return Ok(Pattern::Under(rest.to_string()));
        }
        if !valid_name(&t) {
            return Err(format!("`{text}`: not a host name"));
        }
        Ok(Pattern::Exact(t))
    }

    fn matches(&self, host: &str) -> bool {
        match self {
            Pattern::Exact(h) => h == host,
            Pattern::Under(s) => {
                host.len() > s.len() + 1
                    && host.ends_with(s.as_str())
                    && host.as_bytes()[host.len() - s.len() - 1] == b'.'
            }
        }
    }
}

/// A DNS name: dot-separated labels of letters, digits and hyphens, at least two labels, not all-numeric
/// (so no IPv4 literal), 253 bytes at most.
fn valid_name(h: &str) -> bool {
    if h.is_empty() || h.len() > 253 || !h.contains('.') {
        return false;
    }
    let mut all_numeric = true;
    for label in h.split('.') {
        if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return false;
        }
        if !label.bytes().all(|b| b.is_ascii_digit()) {
            all_numeric = false;
        }
    }
    !all_numeric
}

/// Why a request was refused. `as_str` is the word the deny log carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Not a request we could read in full, or not `CONNECT host:port HTTP/1.x`.
    BadRequest,
    /// Any method but `CONNECT`.
    BadMethod,
    /// Any port but 443.
    BadPort,
    /// The host is not on the group's list (an IP literal is never on it).
    NotListed,
    /// The name resolved only to loopback, private or link-local addresses.
    BadAddress,
    /// The name did not resolve, or nothing answered.
    Unreachable,
    /// Too many tunnels open.
    Busy,
    /// The TLS ClientHello names a different host than the `CONNECT` (or none): domain fronting through an allowed name.
    SniMismatch,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::BadRequest => "bad-request",
            Reason::BadMethod => "bad-method",
            Reason::BadPort => "bad-port",
            Reason::NotListed => "not-listed",
            Reason::BadAddress => "bad-address",
            Reason::Unreachable => "unreachable",
            Reason::Busy => "busy",
            Reason::SniMismatch => "sni-mismatch",
        }
    }
}

/// A group's policy: the names it may reach.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    patterns: Vec<Pattern>,
    /// Tests only: accept loopback and private resolved addresses (a local upstream stands in for the internet).
    pub allow_private_addresses: bool,
}

impl Policy {
    pub fn new(patterns: Vec<Pattern>) -> Policy {
        Policy {
            patterns,
            allow_private_addresses: false,
        }
    }

    /// Parse a list of `host` / `*.suffix` entries.
    pub fn parse_list<S: AsRef<str>>(entries: &[S]) -> Result<Policy, String> {
        let mut v = Vec::new();
        for e in entries {
            v.push(Pattern::parse(e.as_ref())?);
        }
        Ok(Policy::new(v))
    }

    pub fn allows(&self, host: &str) -> bool {
        self.patterns.iter().any(|p| p.matches(host))
    }

    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }
}

/// The outcome of reading one request head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow {
        host: String,
    },
    Deny {
        reason: Reason,
        host: String,
        port: u16,
    },
}

/// Decide on a request head (everything up to the blank line). Only the first line is read.
pub fn decide(policy: &Policy, head: &str) -> Decision {
    let deny = |reason: Reason, host: &str, port: u16| Decision::Deny {
        reason,
        host: sanitize(host),
        port,
    };
    let line = head.split("\r\n").next().unwrap_or("");
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return deny(Reason::BadRequest, "", 0);
    };
    if !(version == "HTTP/1.1" || version == "HTTP/1.0") {
        return deny(Reason::BadRequest, "", 0);
    }
    if method != "CONNECT" {
        return deny(Reason::BadMethod, target, 0);
    }
    // The target is `host:port` only: no scheme, no userinfo, no path, no brackets.
    let Some((host, port_text)) = target.rsplit_once(':') else {
        return deny(Reason::BadRequest, target, 0);
    };
    // digits only and no leading zero, so `0443` cannot read as 443 to one parser and differ to another
    if port_text.is_empty()
        || port_text.starts_with('0')
        || !port_text.bytes().all(|b| b.is_ascii_digit())
    {
        return deny(Reason::BadRequest, target, 0);
    }
    let Ok(port) = port_text.parse::<u16>() else {
        return deny(Reason::BadRequest, target, 0);
    };
    if port != ALLOWED_PORT {
        return deny(Reason::BadPort, host, port);
    }
    let host = host.to_ascii_lowercase();
    if !valid_name(&host) || !policy.allows(&host) {
        return deny(Reason::NotListed, &host, port);
    }
    Decision::Allow { host }
}

/// Keep a logged host name to printable, space-free ASCII and 253 bytes, so a hostile request cannot forge a log line.
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_graphic())
        .take(253)
        .collect()
}

/// A resolved address is usable only if it is not the host's own or a private network.
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            !(a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_multicast()
                || a.octets()[0] == 0
                || (a.octets()[0] == 100 && (a.octets()[1] & 0xc0) == 64))
        }
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return public_address(IpAddr::V4(v4));
            }
            let s = a.segments();
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// One line of the deny log: time, group, host, port, reason, guest address. Never a payload or a header.
pub fn deny_line(
    unix_secs: u64,
    group: &str,
    host: &str,
    port: u16,
    reason: Reason,
    peer: &str,
) -> String {
    format!(
        "{unix_secs} deny group={} host={} port={port} reason={} peer={}",
        sanitize(group),
        sanitize(host),
        reason.as_str(),
        sanitize(peer)
    )
}

/// One line for a closed tunnel: host and bytes each way.
pub fn close_line(
    unix_secs: u64,
    group: &str,
    host: &str,
    up: u64,
    down: u64,
    peer: &str,
) -> String {
    format!(
        "{unix_secs} close group={} host={} up={up} down={down} peer={}",
        sanitize(group),
        sanitize(host),
        sanitize(peer)
    )
}

/// Where log lines go. `Fn` so a test can collect them; the real one appends to a file the runner accounts cannot read.
pub type Log = Arc<dyn Fn(String) + Send + Sync>;

/// How one proxy listener is set up.
#[derive(Clone)]
pub struct Listener {
    pub group: String,
    pub policy: Policy,
    pub log: Log,
    /// Tunnels open at once (further requests are refused `busy`).
    pub max_tunnels: usize,
    /// A connection that sends nothing for this long is dropped.
    pub idle: Duration,
    /// Read the first TLS record and refuse the tunnel unless its SNI is the `CONNECT` host. Off by default: a client using
    /// Encrypted Client Hello sends no usable SNI and would be refused.
    pub check_sni: bool,
}

impl fmt::Debug for Listener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Listener")
            .field("group", &self.group)
            .field("policy", &self.policy)
            .field("max_tunnels", &self.max_tunnels)
            .finish()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read until the blank line that ends the head. `None` when it is too long, closes early or times out.
fn read_head(s: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while buf.len() < MAX_HEAD {
        match s.read(&mut byte) {
            Ok(1) => buf.push(byte[0]),
            _ => return None,
        }
        if buf.ends_with(b"\r\n\r\n") {
            return String::from_utf8(buf).ok();
        }
    }
    None
}

/// The server name in a TLS ClientHello record (`record` starts at the record header), or `None` if it is not a ClientHello,
/// is cut short, or carries no `server_name`. Reads only what the protocol defines; never indexes past the buffer.
pub fn parse_sni(record: &[u8]) -> Option<String> {
    struct Cur<'a>(&'a [u8]);
    impl<'a> Cur<'a> {
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            if self.0.len() < n {
                return None;
            }
            let (a, b) = self.0.split_at(n);
            self.0 = b;
            Some(a)
        }
        fn u8(&mut self) -> Option<usize> {
            self.take(1).map(|b| usize::from(b[0]))
        }
        fn u16(&mut self) -> Option<usize> {
            self.take(2)
                .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
        }
        fn u24(&mut self) -> Option<usize> {
            self.take(3)
                .map(|b| (usize::from(b[0]) << 16) | (usize::from(b[1]) << 8) | usize::from(b[2]))
        }
    }
    let mut c = Cur(record);
    if c.u8()? != 0x16 {
        return None; // not a handshake record
    }
    c.take(2)?; // record version
    let rec_len = c.u16()?;
    let mut h = Cur(c.take(rec_len)?);
    if h.u8()? != 1 {
        return None; // not a ClientHello
    }
    let hs_len = h.u24()?;
    let mut b = Cur(h.take(hs_len)?);
    b.take(2 + 32)?; // client version, random
    let sid = b.u8()?;
    b.take(sid)?;
    let cs = b.u16()?;
    b.take(cs)?;
    let comp = b.u8()?;
    b.take(comp)?;
    let ext_len = b.u16()?;
    let mut e = Cur(b.take(ext_len)?);
    while let (Some(kind), Some(len)) = (e.u16(), e.u16()) {
        let body = e.take(len)?;
        if kind == 0 {
            let mut n = Cur(body);
            let list = n.u16()?;
            let mut l = Cur(n.take(list)?);
            while let (Some(t), Some(nl)) = (l.u8(), l.u16()) {
                let name = l.take(nl)?;
                if t == 0 {
                    return String::from_utf8(name.to_vec())
                        .ok()
                        .map(|s| s.to_ascii_lowercase());
                }
            }
            return None;
        }
    }
    None
}

/// Does the first TLS record name `host`? With the check on, a missing or different name is a mismatch.
pub fn sni_matches(host: &str, record: &[u8]) -> bool {
    parse_sni(record).is_some_and(|n| n == host)
}

/// Read one whole TLS record (header plus body) from the guest, at most `MAX_HELLO` bytes.
fn read_record(s: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 5];
    s.read_exact(&mut buf).ok()?;
    let len = usize::from(u16::from_be_bytes([buf[3], buf[4]]));
    if buf[0] != 0x16 || len == 0 || len > MAX_HELLO {
        return Some(buf); // not a handshake record we can judge: the caller refuses it
    }
    buf.resize(5 + len, 0);
    s.read_exact(&mut buf[5..]).ok()?;
    Some(buf)
}

fn relay(mut from: TcpStream, mut to: TcpStream, counter: Arc<Mutex<u64>>) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
                if let Ok(mut c) = counter.lock() {
                    *c += n as u64;
                }
            }
        }
    }
    let _ = to.shutdown(std::net::Shutdown::Write);
}

fn connect_upstream(host: &str, policy: &Policy) -> Result<TcpStream, Reason> {
    let addrs: Vec<SocketAddr> = (host, ALLOWED_PORT)
        .to_socket_addrs()
        .map_err(|_| Reason::Unreachable)?
        .collect();
    if addrs.is_empty() {
        return Err(Reason::Unreachable);
    }
    let usable: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|a| policy.allow_private_addresses || public_address(a.ip()))
        .collect();
    if usable.is_empty() {
        return Err(Reason::BadAddress);
    }
    for a in usable {
        if let Ok(s) = TcpStream::connect_timeout(&a, Duration::from_secs(10)) {
            return Ok(s);
        }
    }
    Err(Reason::Unreachable)
}

fn handle(mut guest: TcpStream, cfg: &Listener, open: &AtomicUsize) {
    let peer = guest
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let _ = guest.set_read_timeout(Some(cfg.idle));
    let refuse = |guest: &mut TcpStream, reason: Reason, host: &str, port: u16| {
        (cfg.log)(deny_line(now(), &cfg.group, host, port, reason, &peer));
        let _ = guest
            .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
    };
    let Some(head) = read_head(&mut guest) else {
        refuse(&mut guest, Reason::BadRequest, "", 0);
        return;
    };
    let host = match decide(&cfg.policy, &head) {
        Decision::Allow { host } => host,
        Decision::Deny { reason, host, port } => {
            refuse(&mut guest, reason, &host, port);
            return;
        }
    };
    if open.fetch_add(1, Ordering::SeqCst) >= cfg.max_tunnels {
        open.fetch_sub(1, Ordering::SeqCst);
        refuse(&mut guest, Reason::Busy, &host, ALLOWED_PORT);
        return;
    }
    let upstream = connect_upstream(&host, &cfg.policy);
    let Ok(up) = upstream else {
        open.fetch_sub(1, Ordering::SeqCst);
        refuse(
            &mut guest,
            upstream.err().unwrap_or(Reason::Unreachable),
            &host,
            ALLOWED_PORT,
        );
        return;
    };
    let mut up = up;
    let first = if cfg.check_sni {
        // the client speaks only after the 200: answer, read its first record, and judge it before one byte goes upstream
        if guest
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .is_err()
        {
            open.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        match read_record(&mut guest) {
            Some(rec) if sni_matches(&host, &rec) => Some(rec),
            _ => {
                (cfg.log)(deny_line(
                    now(),
                    &cfg.group,
                    &host,
                    ALLOWED_PORT,
                    Reason::SniMismatch,
                    &peer,
                ));
                open.fetch_sub(1, Ordering::SeqCst);
                return;
            }
        }
    } else {
        None
    };
    let _ = guest.set_read_timeout(None);
    let said = match &first {
        Some(rec) => up.write_all(rec).is_ok(),
        None => guest
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .is_ok(),
    };
    if said {
        let (up_bytes, down_bytes) = (Arc::new(Mutex::new(0u64)), Arc::new(Mutex::new(0u64)));
        if let (Ok(g2), Ok(u2)) = (guest.try_clone(), up.try_clone()) {
            let c = up_bytes.clone();
            let t = std::thread::spawn(move || relay(g2, u2, c));
            relay(up, guest, down_bytes.clone());
            let _ = t.join();
        }
        let u = up_bytes.lock().map(|g| *g).unwrap_or(0);
        let d = down_bytes.lock().map(|g| *g).unwrap_or(0);
        (cfg.log)(close_line(now(), &cfg.group, &host, u, d, &peer));
    }
    open.fetch_sub(1, Ordering::SeqCst);
}

/// Accept connections on `listener` forever (one thread per connection). The caller binds the listener to the pool
/// switch's host address, never `0.0.0.0`.
pub fn serve(listener: TcpListener, cfg: Listener) {
    let open = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let (cfg, open) = (cfg.clone(), open.clone());
        std::thread::spawn(move || handle(conn, &cfg, &open));
    }
}

/// May a listener bind to `ip`? Only a private IPv4 address (a pool switch's host address, `10.77.x.1`) or loopback:
/// never `0.0.0.0`, `::`, a public or a link-local address, so the proxy cannot be reached from the LAN by accident.
pub fn bindable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => !a.is_unspecified() && (a.is_private() || a.is_loopback()),
        IpAddr::V6(_) => false,
    }
}

/// A [`Log`] that appends lines to `path`; when the file passes `cap` bytes it is renamed to `path.1` (replacing the
/// previous one) and a new file starts. Write failures are swallowed: the proxy must not stop because its log did.
pub fn file_log(path: std::path::PathBuf, cap: u64) -> Log {
    let lock = Mutex::new(());
    Arc::new(move |line: String| {
        let _g = lock.lock();
        if std::fs::metadata(&path)
            .map(|m| m.len() >= cap)
            .unwrap_or(false)
        {
            let mut old = path.clone().into_os_string();
            old.push(".1");
            let _ = std::fs::rename(&path, old);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{line}");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol() -> Policy {
        Policy::parse_list(&[
            "github.com",
            "api.github.com",
            "*.actions.githubusercontent.com",
            "index.crates.io",
        ])
        .unwrap()
    }

    fn denied(d: Decision) -> Reason {
        match d {
            Decision::Deny { reason, .. } => reason,
            Decision::Allow { host } => panic!("allowed {host}"),
        }
    }

    #[test]
    fn listed_hosts_on_443_are_allowed() {
        for h in [
            "github.com",
            "API.GitHub.com",
            "pipelines.actions.githubusercontent.com",
            "index.crates.io",
        ] {
            let d = decide(
                &pol(),
                &format!("CONNECT {h}:443 HTTP/1.1\r\nHost: {h}:443\r\n\r\n"),
            );
            assert!(matches!(d, Decision::Allow { .. }), "{h}: {d:?}");
        }
    }

    #[test]
    fn known_bad_unlisted_host_is_refused() {
        assert_eq!(
            denied(decide(&pol(), "CONNECT example.com:443 HTTP/1.1\r\n\r\n")),
            Reason::NotListed
        );
        // a list missing a host refuses it: the same request passes once the host is listed
        let wider = Policy::parse_list(&["example.com"]).unwrap();
        assert!(matches!(
            decide(&wider, "CONNECT example.com:443 HTTP/1.1\r\n\r\n"),
            Decision::Allow { .. }
        ));
    }

    #[test]
    fn wildcard_needs_a_label_and_never_matches_its_own_apex() {
        let p = pol();
        assert_eq!(
            denied(decide(
                &p,
                "CONNECT actions.githubusercontent.com:443 HTTP/1.1\r\n\r\n"
            )),
            Reason::NotListed
        );
        assert_eq!(
            denied(decide(
                &p,
                "CONNECT evilactions.githubusercontent.com:443 HTTP/1.1\r\n\r\n"
            )),
            Reason::NotListed
        );
        assert_eq!(
            denied(decide(
                &p,
                "CONNECT actions.githubusercontent.com.evil.com:443 HTTP/1.1\r\n\r\n"
            )),
            Reason::NotListed
        );
        assert!(Pattern::parse("*").is_err());
        assert!(Pattern::parse("*.com").is_err());
        assert!(Pattern::parse("a.*.com").is_err());
        assert!(Pattern::parse("github.com:443").is_err());
        assert!(Pattern::parse("1.2.3.4").is_err());
        assert!(Pattern::parse("*.github.com").is_ok());
    }

    #[test]
    fn only_port_443() {
        assert_eq!(
            denied(decide(&pol(), "CONNECT github.com:80 HTTP/1.1\r\n\r\n")),
            Reason::BadPort
        );
        assert_eq!(
            denied(decide(&pol(), "CONNECT github.com:22 HTTP/1.1\r\n\r\n")),
            Reason::BadPort
        );
        assert_eq!(
            denied(decide(&pol(), "CONNECT github.com:0443 HTTP/1.1\r\n\r\n")),
            Reason::BadRequest
        );
        assert_eq!(
            denied(decide(&pol(), "CONNECT github.com:+443 HTTP/1.1\r\n\r\n")),
            Reason::BadRequest
        );
    }

    #[test]
    fn only_connect() {
        for m in ["GET", "POST", "HEAD", "connect", "PUT"] {
            assert_eq!(
                denied(decide(
                    &pol(),
                    &format!("{m} github.com:443 HTTP/1.1\r\n\r\n")
                )),
                Reason::BadMethod,
                "{m}"
            );
        }
        assert_eq!(
            denied(decide(&pol(), "GET http://github.com/ HTTP/1.1\r\n\r\n")),
            Reason::BadMethod
        );
    }

    #[test]
    fn malformed_requests_fail_closed() {
        for h in [
            "",
            "\r\n\r\n",
            "CONNECT github.com:443\r\n\r\n",
            "CONNECT github.com:443 HTTP/2\r\n\r\n",
            "CONNECT github.com:443 HTTP/1.1 extra\r\n\r\n",
            "CONNECT  github.com:443 HTTP/1.1\r\n\r\n",
            "CONNECT github.com HTTP/1.1\r\n\r\n",
            "CONNECT github.com:abc HTTP/1.1\r\n\r\n",
            "CONNECT github.com:99999 HTTP/1.1\r\n\r\n",
        ] {
            assert_eq!(denied(decide(&pol(), h)), Reason::BadRequest, "{h:?}");
        }
    }

    #[test]
    fn ip_literals_userinfo_and_odd_names_are_never_listed() {
        for t in [
            "1.1.1.1:443",
            "[::1]:443",
            "127.0.0.1:443",
            "user@github.com:443",
            "github.com.:443",
            "git hub.com:443",
            "-a.github.com:443",
            "github..com:443",
        ] {
            let d = decide(&pol(), &format!("CONNECT {t} HTTP/1.1\r\n\r\n"));
            assert!(matches!(d, Decision::Deny { .. }), "{t}: {d:?}");
        }
    }

    #[test]
    fn log_lines_cannot_be_forged() {
        let d = decide(
            &pol(),
            "CONNECT evil\u{7}.com\r\nforged:443 HTTP/1.1\r\n\r\n",
        );
        if let Decision::Deny { host, .. } = &d {
            assert!(!host.contains('\n') && !host.contains('\r'));
        }
        let line = deny_line(1, "g\nx", "h o\r\nst", 443, Reason::NotListed, "10.0.0.2");
        assert_eq!(line.lines().count(), 1, "{line}");
        assert_eq!(
            line,
            "1 deny group=gx host=host port=443 reason=not-listed peer=10.0.0.2"
        );
    }

    #[test]
    fn host_addresses_are_not_reachable() {
        for ip in [
            "127.0.0.1",
            "10.77.0.1",
            "192.168.1.5",
            "172.16.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!public_address(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["140.82.112.3", "1.1.1.1", "2606:4700::1111"] {
            assert!(public_address(ip.parse().unwrap()), "{ip}");
        }
    }

    // ---- sockets: a local upstream stands in for the internet (loopback allowed only in these tests) ----

    fn collect() -> (Log, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let l2 = lines.clone();
        (Arc::new(move |s| l2.lock().unwrap().push(s)), lines)
    }

    fn start(policy: Policy) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let (log, lines) = collect();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let cfg = Listener {
            group: "g1".into(),
            policy,
            log,
            max_tunnels: 8,
            idle: Duration::from_secs(5),
            check_sni: false,
        };
        std::thread::spawn(move || serve(l, cfg));
        (addr, lines)
    }

    fn ask(proxy: SocketAddr, request: &str) -> String {
        let mut s = TcpStream::connect(proxy).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut out = Vec::new();
        let mut b = [0u8; 256];
        loop {
            match s.read(&mut b) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    out.extend_from_slice(&b[..n]);
                    if out.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn refused_request_gets_403_and_one_deny_line() {
        let (proxy, lines) = start(pol());
        let r = ask(proxy, "CONNECT example.com:443 HTTP/1.1\r\n\r\n");
        assert!(r.starts_with("HTTP/1.1 403"), "{r}");
        let r = ask(proxy, "GET / HTTP/1.1\r\n\r\n");
        assert!(r.starts_with("HTTP/1.1 403"), "{r}");
        std::thread::sleep(Duration::from_millis(200));
        let l = lines.lock().unwrap().clone();
        assert_eq!(l.len(), 2, "{l:?}");
        assert!(
            l[0].contains(
                "deny group=g1 host=example.com port=443 reason=not-listed peer=127.0.0.1"
            ),
            "{}",
            l[0]
        );
        assert!(l[1].contains("reason=bad-method"), "{}", l[1]);
    }

    #[test]
    fn a_name_that_resolves_to_the_host_is_refused() {
        // 127.0.0.1 stands for a listed name whose DNS answer points at the host itself: the address rule refuses it
        let p = Policy::parse_list(&["example.com"]).unwrap();
        assert!(!p.allow_private_addresses);
        assert!(matches!(
            connect_upstream("127.0.0.1", &p),
            Err(Reason::BadAddress)
        ));
        // control: the same call with the test switch on does not refuse on the address (it only fails to connect on :443)
        let mut q = p.clone();
        q.allow_private_addresses = true;
        assert!(!matches!(
            connect_upstream("127.0.0.1", &q),
            Err(Reason::BadAddress)
        ));
    }

    #[test]
    fn a_listed_host_is_tunnelled_and_the_close_line_counts_bytes() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let uaddr = upstream.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = upstream.accept() {
                let mut b = [0u8; 5];
                if c.read_exact(&mut b).is_ok() {
                    let _ = c.write_all(b"pong!");
                }
            }
        });
        // the relay and the close line, on local sockets (`serve` itself resolves a name on :443, which a test cannot stand in for)
        let (log, lines) = collect();
        let (a, b) = (
            TcpListener::bind("127.0.0.1:0").unwrap(),
            TcpStream::connect(uaddr).unwrap(),
        );
        let a_addr = a.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut g = TcpStream::connect(a_addr).unwrap();
            g.write_all(b"ping!").unwrap();
            let mut r = [0u8; 5];
            g.read_exact(&mut r).unwrap();
            r
        });
        let (guest_side, _) = a.accept().unwrap();
        let up = Arc::new(Mutex::new(0u64));
        let down = Arc::new(Mutex::new(0u64));
        let (g2, u2) = (guest_side.try_clone().unwrap(), b.try_clone().unwrap());
        let c = up.clone();
        let th = std::thread::spawn(move || relay(g2, u2, c));
        relay(b, guest_side, down.clone());
        th.join().unwrap();
        assert_eq!(&t.join().unwrap(), b"pong!");
        assert_eq!((*up.lock().unwrap(), *down.lock().unwrap()), (5, 5));
        log(close_line(1, "g1", "github.com", 5, 5, "10.0.0.2"));
        assert_eq!(
            lines.lock().unwrap()[0],
            "1 close group=g1 host=github.com up=5 down=5 peer=10.0.0.2"
        );
    }

    #[test]
    fn bindable_addresses_are_the_switch_side_only() {
        for ok in ["10.77.0.1", "192.168.50.1", "127.0.0.1"] {
            assert!(bindable(ok.parse().unwrap()), "{ok}");
        }
        for bad in ["0.0.0.0", "8.8.8.8", "169.254.1.1", "::", "::1", "fe80::1"] {
            assert!(!bindable(bad.parse().unwrap()), "{bad}");
        }
    }

    #[test]
    fn the_file_log_appends_and_rotates_at_the_cap() {
        let dir = std::env::temp_dir().join(format!("antspawn-egress-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deny.log");
        let log = file_log(path.clone(), 40);
        log("first line, long enough to pass the cap".into());
        log("second".into());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "second
"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("deny.log.1")).unwrap(),
            "first line, long enough to pass the cap
"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A minimal TLS 1.2 ClientHello record naming `sni` (or no server_name extension when `None`).
    fn hello(sni: Option<&str>) -> Vec<u8> {
        let mut ext = Vec::new();
        if let Some(n) = sni {
            let mut entry = vec![0u8];
            entry.extend((n.len() as u16).to_be_bytes());
            entry.extend(n.as_bytes());
            let mut list = (entry.len() as u16).to_be_bytes().to_vec();
            list.extend(entry);
            ext.extend([0u8, 0]);
            ext.extend((list.len() as u16).to_be_bytes());
            ext.extend(list);
        }
        ext.extend([0u8, 0x0b, 0, 2, 1, 0]); // an unrelated extension after it
        let mut body = vec![3u8, 3];
        body.extend([7u8; 32]);
        body.push(0); // session id
        body.extend([0u8, 2, 0x13, 0x01]); // one cipher suite
        body.extend([1u8, 0]); // compression
        body.extend((ext.len() as u16).to_be_bytes());
        body.extend(ext);
        let mut hs = vec![1u8];
        hs.extend(&(body.len() as u32).to_be_bytes()[1..]);
        hs.extend(body);
        let mut rec = vec![0x16u8, 3, 1];
        rec.extend((hs.len() as u16).to_be_bytes());
        rec.extend(hs);
        rec
    }

    #[test]
    fn the_sni_is_read_from_a_client_hello_and_compared_with_the_connect_host() {
        let h = hello(Some("API.GitHub.com"));
        assert_eq!(parse_sni(&h).as_deref(), Some("api.github.com"));
        assert!(sni_matches("api.github.com", &h));
        // known-bad controls: a different name (fronting), no name, not a hello, a cut-short record
        assert!(!sni_matches("github.com", &h));
        assert!(!sni_matches("api.github.com", &hello(None)));
        assert!(!sni_matches("api.github.com", b"GET / HTTP/1.1\r\n\r\n"));
        for cut in [0, 1, 4, 5, 9, 20, 60, h.len() - 1] {
            assert_eq!(parse_sni(&h[..cut]), None, "cut at {cut}");
        }
    }

    #[test]
    fn a_hostile_hello_never_panics() {
        let h = hello(Some("a.example.org"));
        for i in 0..h.len() {
            let mut x = h.clone();
            x[i] = 0xff;
            let _ = parse_sni(&x);
            let mut y = h.clone();
            y.truncate(i);
            let _ = parse_sni(&y);
        }
    }

    #[test]
    fn a_hello_whose_name_is_not_the_connect_host_is_the_sni_mismatch_reason() {
        assert_eq!(Reason::SniMismatch.as_str(), "sni-mismatch");
        let line = deny_line(1, "g", "github.com", 443, Reason::SniMismatch, "10.0.0.2");
        assert!(line.contains("reason=sni-mismatch"), "{line}");
    }
}
