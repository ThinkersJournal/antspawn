// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-broker`: the VM broker (slice V3 of `docs/SANDBOX-BROKER.md`).
//!
//! * `spawn-broker --config FILE --check`: read and check the configuration (see
//!   `antspawn::broker_config`), print what it offers, change nothing, touch no Hyper-V.
//! * `spawn-broker --config FILE`: serve. Removes leftover `antspawn-` VMs from a predecessor, then
//!   accepts clients on the configured pipe. Each client's SID is read from the OS; the VMs a
//!   connection made are destroyed when it ends. A thread expires VMs past their lifetime and retries
//!   deletes that failed.
//!
//! No shutdown command: stopping the process leaves its VMs until the next start sweeps them. Run it
//! as the limited broker account (V7); it never elevates and never creates accounts.
//!
//! UNVERIFIED end to end: serving has not been run against real Hyper-V, a real second account or a
//! real guest. Exit 0 = ok, 1 = a failure (printed), 2 = usage.

use std::process::ExitCode;

#[cfg(windows)]
mod serve {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    use antspawn::broker::Broker;
    use antspawn::broker_config::BrokerConfig;
    use antspawn::broker_pipe::Listener;
    use antspawn::broker_serve::serve_connection;
    use antspawn::hyperv::{HyperV, PowerShell};

    /// Most connections served at once; the rest are dropped at accept. An allowed account that
    /// opens connections and sits idle can hold this many threads, and no more.
    const MAX_CONNECTIONS: usize = 32;
    const TICK: Duration = Duration::from_secs(5);

    type Shared = Arc<Mutex<Broker<HyperV<PowerShell>>>>;

    /// A poisoned lock means a request handler panicked part way through a change: the broker's
    /// books may no longer match Hyper-V. Stop; the next start sweeps every `antspawn-` VM.
    fn lock(b: &Shared) -> MutexGuard<'_, Broker<HyperV<PowerShell>>> {
        b.lock().unwrap_or_else(|_| {
            eprintln!("spawn-broker: internal error, stopping (the next start removes leftovers)");
            std::process::exit(1)
        })
    }

    pub fn run(cfg: BrokerConfig) -> Result<(), String> {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        let driver = HyperV::new(PowerShell, cfg.hyperv.clone());
        let broker = Broker::new(driver, cfg.policy, cfg.limits, cfg.slots, seed)?;
        let broker: Shared = Arc::new(Mutex::new(broker));
        let mut listener = Listener::new(&cfg.pipe, &cfg.allow_sids)?;
        let start = Instant::now();
        println!("spawn-broker: serving on pipe {}", cfg.pipe);

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
                    eprintln!("spawn-broker: accept: {e}");
                    continue;
                }
            };
            if live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                live.fetch_sub(1, Ordering::SeqCst);
                eprintln!("spawn-broker: connection limit reached; one client dropped");
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
                eprintln!("spawn-broker: connection {conn} ended: {ended:?}");
                live.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: spawn-broker --config FILE [--check]");
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
        .and_then(|t| antspawn::broker_config::BrokerConfig::from_toml(&t))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spawn-broker: {e}");
            return ExitCode::from(1);
        }
    };
    if check {
        println!(
            "config ok: pipe {}, {} allowed SID(s), images {:?}, {} profile(s), {} slot(s), max_total {}",
            cfg.pipe,
            cfg.allow_sids.len(),
            cfg.policy.images,
            cfg.policy.profiles.len(),
            cfg.slots.len(),
            cfg.limits.max_total
        );
        return ExitCode::SUCCESS;
    }
    #[cfg(windows)]
    {
        match serve::run(cfg) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("spawn-broker: {e}");
                ExitCode::from(1)
            }
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("spawn-broker serves on Windows only (the config was checked)");
        ExitCode::from(2)
    }
}
