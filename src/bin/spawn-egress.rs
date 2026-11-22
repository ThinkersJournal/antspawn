// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-egress`: the egress filtering proxy (`docs/RUNNER-NETWORK.md`, ROADMAP row 38), one listener per runner group.
//!
//! `spawn-egress --listen 10.77.0.1:3128 --group NAME [--allow host:443]... [--approved egress-approved.toml --repos O/a,O/b] [--check-sni] --log deny.log`
//!
//! It binds only a private IPv4 or loopback address (never `0.0.0.0`), allows the default hosts plus approved extras,
//! answers `CONNECT host:443` and nothing else, and logs every refusal as one line. It does not elevate and creates nothing
//! but its log file. Not yet started by anything: the pool switches and firewall rules (row 39) and the launcher (row 40) come first.

use antspawn::adapter::egress_policy::{Approvals, DEFAULT_ALLOW, entry_host};
use antspawn::egress::{Listener, Policy, bindable, file_log, serve};
use std::net::{SocketAddr, TcpListener};
use std::process::ExitCode;
use std::time::Duration;

fn usage() -> ExitCode {
    eprintln!(
        "usage: spawn-egress --listen <ip:port> --group <name> --log <file> [--allow host:443]... [--approved <egress-approved.toml> --repos O/a,O/b]\n  every --allow entry needs an approval in the file for each repo; the listen address must be a private IPv4 or loopback"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut listen, mut group, mut log, mut approved, mut repos) =
        (None, None, None, None, Vec::new());
    let mut allow: Vec<String> = Vec::new();
    let mut check_sni = false;
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned();
        match (args[i].as_str(), v) {
            ("--listen", Some(v)) => listen = Some(v),
            ("--group", Some(v)) => group = Some(v),
            ("--log", Some(v)) => log = Some(v),
            ("--approved", Some(v)) => approved = Some(v),
            ("--repos", Some(v)) => repos = v.split(',').map(str::to_string).collect(),
            ("--allow", Some(v)) => allow.push(v),
            ("--check-sni", _) => {
                check_sni = true;
                i += 1;
                continue;
            }
            _ => return usage(),
        }
        i += 2;
    }
    let (Some(listen), Some(group), Some(log)) = (listen, group, log) else {
        return usage();
    };
    let Ok(addr) = listen.parse::<SocketAddr>() else {
        eprintln!("spawn-egress: `{listen}` is not ip:port");
        return ExitCode::from(2);
    };
    if !bindable(addr.ip()) {
        eprintln!(
            "spawn-egress: refusing to listen on {}: only a private IPv4 or loopback address (never 0.0.0.0)",
            addr.ip()
        );
        return ExitCode::from(2);
    }
    let approvals = match approved {
        None => Approvals::default(),
        Some(p) => match std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|t| Approvals::from_toml(&t))
        {
            Ok(a) => a,
            Err(e) => {
                eprintln!("spawn-egress: {p}: {e}");
                return ExitCode::from(2);
            }
        },
    };
    let mut hosts: Vec<String> = DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect();
    for e in &allow {
        let host = match entry_host(e) {
            Ok(h) => h,
            Err(why) => {
                eprintln!("spawn-egress: {why}");
                return ExitCode::from(2);
            }
        };
        let wildcard = host.starts_with("*.");
        if repos.is_empty()
            || !repos.iter().all(|r| {
                approvals.approved.iter().any(|a| {
                    a.host.eq_ignore_ascii_case(&host) && &a.repo == r && (!wildcard || a.wildcard)
                })
            })
        {
            eprintln!(
                "spawn-egress: `{e}` is not approved for every repo in --repos (give --approved and --repos)"
            );
            return ExitCode::from(2);
        }
        hosts.push(host);
    }
    let policy = match Policy::parse_list(&hosts) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("spawn-egress: {e}");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("spawn-egress: cannot bind {addr}: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!(
        "spawn-egress: group {group} on {addr}, {} hosts allowed, log {log}",
        hosts.len()
    );
    let cfg = Listener {
        group,
        policy,
        log: file_log(log.into(), 4 * 1024 * 1024),
        max_tunnels: 64,
        idle: Duration::from_secs(30),
        check_sni,
    };
    serve(listener, cfg);
    ExitCode::SUCCESS
}
