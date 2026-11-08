// SPDX-License-Identifier: MIT OR Apache-2.0
//! A session on the REAL restricted account, through Task Scheduler: runner-only (slice S3 of
//! `docs/SESSION-PLAN.md`). The pipe, DACL, kill-on-close job and disconnect behaviour are shown
//! against the real host in `tests/session_host.rs`; this adds the one thing that cannot be shown
//! without the account: the host runs as another user and the launcher still reaches it.
//!
//! Until the one-time elevated setup has run (and `spawn-runner.exe` there includes `session`, which
//! needs a redeploy) the test prints `UNVERIFIED` and returns: never `#[ignore]`d. Record real runs
//! in `docs/MEASUREMENTS.md` with date and ref.
#![cfg(windows)]

use antspawn::probes::account;
use antspawn::{ExecRequest, Exit, Launcher};

fn out(o: &antspawn::ExecOutcome) -> String {
    String::from_utf8_lossy(o.streams.stdout.as_deref().unwrap_or_default()).into_owned()
}

#[test]
fn a_session_on_the_account_keeps_state_between_execs_and_leaves_nothing_after_close() {
    if !account::set_up() {
        println!(
            "UNVERIFIED: task AntSpawnRunner is not registered; account setup has not been applied here"
        );
        return;
    }
    let acct = antspawn::launchers::account::Account::default();

    let mut s = match acct.open_session() {
        Ok(s) => s,
        Err(e) => {
            println!("UNVERIFIED: the deployed spawn-runner cannot host a session yet: {e}");
            return;
        }
    };
    let who = s.exec(&ExecRequest::new(["cmd", "/c", "whoami"])).unwrap();
    assert_eq!(who.exit, Exit::Code(0));
    let me = std::process::Command::new("whoami").output().unwrap();
    assert_ne!(
        out(&who).trim().to_lowercase(),
        String::from_utf8_lossy(&me.stdout).trim().to_lowercase(),
        "the session ran as the launcher's own user, not the restricted account"
    );
    assert_eq!(
        s.exec(&ExecRequest::new(["cmd", "/c", "echo kept> f.txt"]))
            .unwrap()
            .exit,
        Exit::Code(0)
    );
    let seen = s
        .exec(&ExecRequest::new(["cmd", "/c", "type f.txt"]))
        .unwrap();
    assert!(
        out(&seen).contains("kept"),
        "state did not persist: {}",
        out(&seen)
    );
    s.close().unwrap();

    // A new session starts from nothing: the file is gone.
    let mut s2 = acct.open_session().unwrap();
    let gone = s2
        .exec(&ExecRequest::new(["cmd", "/c", "type f.txt"]))
        .unwrap();
    assert_ne!(
        gone.exit,
        Exit::Code(0),
        "the previous session's file survived close"
    );
    s2.close().unwrap();
}

/// The two probes the matrix rows `session` and `clean-slate-next-session` for `account` rest on:
/// state persists within a session (so the second probe proves something) and nothing outlives
/// `close`. Same guards as above: UNVERIFIED where the setup or the new `spawn-runner` is missing.
#[test]
fn the_matrix_probes_for_account_sessions_find_no_violation() {
    if !account::set_up() {
        println!(
            "UNVERIFIED: task AntSpawnRunner is not registered; account setup has not been applied here"
        );
        return;
    }
    if let Err(e) = antspawn::launchers::account::Account::default().open_session() {
        println!("UNVERIFIED: the deployed spawn-runner cannot host a session yet: {e}");
        return;
    }
    assert!(
        !account::violates_session(),
        "a file written by one exec was not there for the next"
    );
    assert!(
        !account::violates_clean_slate_session(),
        "a file written in one session existed in the next, after close"
    );
}
