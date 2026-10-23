// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `account` launcher against the REAL restricted account: runner-only. These are the negative
//! controls (read outside, write outside, clean slate, forked survivor, network); `naive` fails them in
//! `tests/negative.rs` and the probes live in `antspawn::probes::account`, so a pass here is a pass that
//! could have failed. `examples/matrix.rs` records the same probes as matrix rows.
//!
//! Until the one-time elevated setup (`scripts/account-setup.ps1 -Apply`) has run on this machine
//! the tests print `UNVERIFIED` and return: they are never `#[ignore]`d, and CI output names them
//! as unverified. Record real runs in `docs/MEASUREMENTS.md` with date and ref.
#![cfg(windows)]

use std::path::Path;

use antspawn::probes::{NET_ADDR, account};

const PROBE: &str = env!("CARGO_BIN_EXE_spawn-probe");

fn set_up() -> bool {
    let ok = account::set_up();
    if !ok {
        println!(
            "UNVERIFIED: task {} is not registered; account setup has not been applied here",
            antspawn::runner::TASK_NAME
        );
    }
    ok
}

#[test]
fn account_cannot_read_a_host_file_outside_its_environment() {
    if set_up() {
        assert!(
            !account::violates_read(Path::new(PROBE)),
            "the probe read the sentinel"
        );
    }
}

#[test]
fn account_cannot_write_a_host_file_outside_its_environment() {
    if set_up() {
        assert!(
            !account::violates_write(Path::new(PROBE)),
            "the probe wrote a host file"
        );
    }
}

#[test]
fn account_leaves_nothing_for_the_next_run() {
    if set_up() {
        assert!(!account::violates_clean_slate(Path::new(PROBE)));
    }
}

#[test]
fn account_kills_a_forked_survivor() {
    if set_up() {
        assert!(
            !account::violates_kill_runaway(Path::new(PROBE)),
            "survivor still alive"
        );
    }
}

#[test]
fn account_has_no_network() {
    if !set_up() {
        return;
    }
    match account::violates_network(Path::new(PROBE), NET_ADDR) {
        Some(v) => assert!(!v, "the probe connected to {NET_ADDR}"),
        None => println!("UNVERIFIED: this host cannot reach {NET_ADDR}, so there is no control"),
    }
}
