// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-sandbox-broker`: the Windows Sandbox broker (Track 2 of `docs/SANDBOX-BROKER.md`).
//!
//! * `spawn-sandbox-broker --config FILE --check`: read and check the configuration (see
//!   `antspawn::sandbox_config`), print what it offers, change nothing, start no sandbox.
//! * `spawn-sandbox-broker --config FILE`: serve. Accepts clients on the configured pipe, reads each
//!   client's SID from the OS, starts a sandbox from a fixed template on request (one at a time), and
//!   stops it when the connection that started it ends. A thread expires sandboxes past their
//!   lifetime and retries stops that failed.
//!
//! No sweep at start: `wsb list` shows every owner's sandboxes, so a broker never stops one it did
//! not start; a crashed broker's sandbox lingers until stopped by hand. Never elevates, never
//! creates accounts.
//!
//! UNVERIFIED end to end: serving has not been run against a real `wsb`, a real second account or a
//! real pipe client. Exit 0 = ok, 1 = a failure (printed), 2 = usage.

use std::process::ExitCode;

#[cfg(windows)]
mod serve {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    use antspawn::broker_pipe::Listener;
    use antspawn::broker_serve::serve_connection;
    use antspawn::sandbox_broker::SandboxBroker;
    use antspawn::sandbox_config::SandboxConfig;
    use antspawn::wsb_driver::{WsbDriver, WsbExe};

    /// Only one sandbox can exist, so few connections are useful; the rest are dropped at accept.
    const MAX_CONNECTIONS: usize = 8;
    const TICK: Duration = Duration::from_secs(5);

    type Shared = Arc<Mutex<SandboxBroker<WsbDriver<WsbExe>>>>;

    /// A poisoned lock means a handler panicked part way through a change: the books may no longer
    /// match `wsb`. Stop rather than guess.
    fn lock(b: &Shared) -> MutexGuard<'_, SandboxBroker<WsbDriver<WsbExe>>> {
        b.lock().unwrap_or_else(|_| {
            eprintln!("spawn-sandbox-broker: internal error, stopping");
            std::process::exit(1)
        })
    }

    pub fn run(cfg: SandboxConfig) -> Result<(), String> {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        let driver = WsbDriver::new(WsbExe, cfg.templates.clone());
        let broker: Shared = Arc::new(Mutex::new(SandboxBroker::new(
            driver, cfg.policy, cfg.limits, seed,
        )));
        let mut listener = Listener::new(&cfg.pipe, &cfg.allow_sids)?;
        let start = Instant::now();
        println!("spawn-sandbox-broker: serving on pipe {}", cfg.pipe);

        let ticker = broker.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(TICK);
                lock(&ticker).tick(start.elapsed());
            }
        });

        let next_conn = AtomicU64::new(1);
        let live = Arc::new(AtomicUsize::new(0));
        loop {
            let (pipe, sid) = match listener.accept() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("spawn-sandbox-broker: accept: {e}");
                    continue;
                }
            };
            if live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                live.fetch_sub(1, Ordering::SeqCst);
                eprintln!("spawn-sandbox-broker: connection limit reached; one client dropped");
                continue;
            }
            let conn = next_conn.fetch_add(1, Ordering::SeqCst);
            let broker = broker.clone();
            let live = live.clone();
            std::thread::spawn(move || {
                let (mut r, mut w) = (&pipe, &pipe);
                let ended = serve_connection(
                    &mut r,
                    &mut w,
                    |request| lock(&broker).handle(conn, &sid, request, start.elapsed()),
                    || lock(&broker).disconnect(conn),
                );
                eprintln!("spawn-sandbox-broker: connection {conn} ended: {ended:?}");
                live.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: spawn-sandbox-broker --config FILE [--check]");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut config = None;
    let mut check = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => match it.next() {
                Some(v) => config = Some(v),
                None => return usage(),
            },
            "--check" => check = true,
            _ => return usage(),
        }
    }
    let Some(path) = config else {
        return usage();
    };
    let cfg = match std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read the config: {e}"))
        .and_then(|t| antspawn::sandbox_config::SandboxConfig::from_toml(&t))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spawn-sandbox-broker: {e}");
            return ExitCode::from(1);
        }
    };
    if check {
        println!(
            "config ok: pipe {}, {} allowed SID(s), profiles {:?}",
            cfg.pipe,
            cfg.allow_sids.len(),
            cfg.policy.sandbox_profiles
        );
        return ExitCode::SUCCESS;
    }
    #[cfg(windows)]
    {
        match serve::run(cfg) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("spawn-sandbox-broker: {e}");
                ExitCode::from(1)
            }
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("spawn-sandbox-broker serves on Windows only (the config was checked)");
        ExitCode::from(2)
    }
}
