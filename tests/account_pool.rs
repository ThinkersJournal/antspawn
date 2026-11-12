// SPDX-License-Identifier: MIT OR Apache-2.0
//! The account pool on REAL restricted accounts: runner-only (slice A3 P3), UNVERIFIED until the
//! per-account setup (P4, CireSnave's elevated paste) has run. Never `#[ignore]`d.
//!
//! Configure with `ANTSPAWN_POOL=account:task,...` (the pool is four accounts, board 420; at least two are needed) and
//! `ANTSPAWN_POOL_PROBE=<path to spawn-probe.exe the pool accounts can run>`; without both the test
//! prints `UNVERIFIED` and returns. Record real runs in `docs/MEASUREMENTS.md` with date and ref.
//!
//! Two properties, each with its known-bad control:
//! * `concurrent`: two sessions on a two-account pool both work; the same pool cut to one account
//!   refuses the second (the probe reports a violation, so it can fail).
//! * `no-read-other-session`: two pool sessions cannot read each other; two sessions FORCED onto one
//!   account (no pool) can, so the probe can fail on this very machine.
#![cfg(windows)]

use antspawn::launchers::account::{Account, AccountIdentity};
use antspawn::pool::AccountPool;
use antspawn::probes::Probes;
use antspawn::runner::Layout;
use antspawn::{LaunchError, Launcher, Session};

fn config() -> Option<(Vec<AccountIdentity>, String)> {
    let pool = std::env::var("ANTSPAWN_POOL").ok()?;
    let probe = std::env::var("ANTSPAWN_POOL_PROBE").ok()?;
    let members: Vec<AccountIdentity> = pool
        .split(',')
        .filter_map(|m| m.split_once(':'))
        .map(|(a, t)| AccountIdentity::new(a.trim(), t.trim()))
        .collect();
    (members.len() >= 2).then_some((members, probe))
}

#[test]
fn the_pool_is_concurrent_and_isolated_and_the_controls_fail() {
    let Some((members, probe)) = config() else {
        println!(
            "UNVERIFIED: ANTSPAWN_POOL / ANTSPAWN_POOL_PROBE not set; the per-account setup (P4) has not been applied here"
        );
        return;
    };
    let layout = Layout::from_env();
    let p = Probes::new(probe);

    let pool = AccountPool::new(layout.clone(), members.clone()).expect("pool");
    let open =
        || -> Result<Box<dyn Session>, LaunchError> { Ok(Box::new(pool.open_session_in(None)?)) };
    assert!(
        !p.violates_concurrent(&open),
        "two sessions did not run at once"
    );
    assert!(
        !p.violates_no_read_other_session(&open),
        "one pool session read another's file"
    );

    // Known-bad control 1: a pool of one cannot run two sessions.
    let one = AccountPool::new(layout.clone(), members[..1].to_vec()).expect("pool of one");
    let open_one =
        || -> Result<Box<dyn Session>, LaunchError> { Ok(Box::new(one.open_session_in(None)?)) };
    assert!(
        p.violates_concurrent(&open_one),
        "a pool of one ran two sessions"
    );

    // Known-bad control 2: both sessions forced onto ONE account read each other.
    let same = Account::with_identity(layout, members[0].clone());
    let open_same = || same.open_session();
    assert!(
        p.violates_no_read_other_session(&open_same),
        "two sessions on one account could not read each other, so the probe cannot fail"
    );
}
