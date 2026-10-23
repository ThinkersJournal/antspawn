// SPDX-License-Identifier: MIT OR Apache-2.0
//! Live proof for the GitHub adapter. Runner-only (needs the App's DPAPI key and the network);
//! prints `MEASURE`/`PROOF` lines for `docs/MEASUREMENTS.md`, never a token, key or JIT config.
//!
//! `cargo run --example github_proof -- probe <adapter.toml>`: exchange the App JWT for an
//! installation token, then list the org's runner groups and runners (read-only).
//!
//! `cargo run --example github_proof -- jit <adapter.toml> <group>`: through [`Minter`] (which
//! verifies the live group first) register one just-in-time runner, see it in the org's list,
//! delete it, and see it gone. No job runs.

#[cfg(windows)]
mod live {
    use std::process::ExitCode;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    use antspawn::adapter::github::{AdapterConfig, JitRequest, MintOutcome, Minter};
    use antspawn::adapter::github_client::{DpapiKeyProvider, HttpGithub};

    fn now_s() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let (Some(mode), Some(cfg_path)) = (args.first(), args.get(1)) else {
            eprintln!("usage: github_proof probe <adapter.toml> | jit <adapter.toml> <group>");
            return ExitCode::from(2);
        };
        let text = match std::fs::read_to_string(cfg_path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("cannot read {cfg_path}: {e}");
                return ExitCode::from(2);
            }
        };
        let cfg = match AdapterConfig::from_toml(&text) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        };
        let keys = match DpapiKeyProvider::new(cfg.key_path.clone(), vec![]) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        };
        let api = HttpGithub::new(cfg.app_id, cfg.installation_id, keys);

        if mode == "queued" {
            use antspawn::adapter::watcher::JobSource;
            let Some(repo) = args.get(2) else {
                eprintln!("usage: github_proof queued <adapter.toml> <owner/repo>");
                return ExitCode::from(2);
            };
            let t = Instant::now();
            return match api.queued(repo) {
                Ok(jobs) => {
                    println!(
                        "PROOF queued({repo}) ok: {} queued job(s) in {} ms",
                        jobs.len(),
                        t.elapsed().as_millis()
                    );
                    for j in &jobs {
                        println!("PROOF job {j:?}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    println!("PROOF queued({repo}) failed: {e}");
                    ExitCode::from(1)
                }
            };
        }

        if mode == "probe" {
            let t = Instant::now();
            match api.list_runners(&cfg.org) {
                Ok(r) => println!(
                    "PROOF token exchange ok; org {} has {} runner(s) (first call incl. JWT+token: {} ms)",
                    cfg.org,
                    r.len(),
                    t.elapsed().as_millis()
                ),
                Err(e) => {
                    println!("PROOF probe failed: {e}");
                    return ExitCode::from(1);
                }
            }
            for g in &cfg.groups {
                use antspawn::adapter::github::GithubApi;
                match api.runner_group(&cfg.org, &g.name) {
                    Ok(i) => println!(
                        "PROOF group `{}`: visibility={} repos={:?}",
                        g.name, i.visibility, i.repos
                    ),
                    Err(e) => println!("PROOF group `{}`: {e}", g.name),
                }
            }
            return ExitCode::SUCCESS;
        }

        let Some(group) = args
            .get(2)
            .and_then(|n| cfg.groups.iter().find(|g| &g.name == n))
        else {
            eprintln!("group not in the config");
            return ExitCode::from(2);
        };
        let name = format!("antspawn-proof-{}", now_s());
        let req = JitRequest {
            org: cfg.org.clone(),
            group: group.name.clone(),
            runner_name: name.clone(),
            labels: group.labels.clone(),
        };
        let mut minter = Minter::new(&api, Default::default());
        let t = Instant::now();
        let jit = match minter.mint(now_s(), group, &req) {
            MintOutcome::Minted(s) => {
                println!(
                    "PROOF minted runner `{name}` in {} ms (jit config {} bytes, not shown)",
                    t.elapsed().as_millis(),
                    s.expose().len()
                );
                s
            }
            MintOutcome::StayQueued {
                reason, operator, ..
            } => {
                println!("PROOF not minted (operator needed: {operator}): {reason}");
                return ExitCode::from(1);
            }
        };
        drop(jit);

        let find = |label: &str| match api.list_runners(&cfg.org) {
            Ok(rs) => rs.into_iter().find(|r| r.name == name),
            Err(e) => {
                println!("PROOF list failed ({label}): {e}");
                None
            }
        };
        let Some(r) = find("after mint") else {
            println!("PROOF runner `{name}` is NOT in the org's list after minting");
            return ExitCode::from(1);
        };
        println!(
            "PROOF registered: id={} status={} busy={}",
            r.id, r.status, r.busy
        );
        if let Err(e) = api.delete_runner(&cfg.org, r.id) {
            println!("PROOF delete failed: {e}");
            return ExitCode::from(1);
        }
        let gone = find("after delete").is_none();
        println!("PROOF removed: runner `{name}` absent from the list = {gone}");
        if gone {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    live::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("github_proof needs the Windows DPAPI key file; run it on the watcher box");
}
