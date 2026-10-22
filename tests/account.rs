// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `account` launcher against the REAL restricted account: runner-only. These are the three
//! negative controls (read outside, clean slate, forked survivor); `naive` fails all three in
//! `tests/negative.rs`, so a pass here is a pass that could have failed.
//!
//! Until the one-time elevated setup (`scripts/account-setup.ps1 -Apply`) has run on this machine
//! the tests print `UNVERIFIED` and return: they are never `#[ignore]`d, and CI output names them
//! as unverified. Record real runs in `docs/MEASUREMENTS.md` with date and ref.
#![cfg(windows)]

use std::process::Command;

use antspawn::launchers::Account;
use antspawn::runner::TASK_NAME;
use antspawn::{Exit, JobSpec, Launcher};

const PROBE: &str = env!("CARGO_BIN_EXE_spawn-probe");

fn set_up() -> bool {
    let ok = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        println!(
            "UNVERIFIED: task {TASK_NAME} is not registered; account setup has not been applied here"
        );
    }
    ok
}

fn spec(args: &[&str], extra: &str) -> JobSpec {
    let cmd: Vec<String> = std::iter::once("spawn-probe.exe")
        .chain(args.iter().copied())
        .map(|a| format!("'{a}'"))
        .collect();
    JobSpec::from_toml(&format!(
        "[job]\nname = \"p\"\ncmd = [{}]\n[limits]\ntime_s = 30\n[[mount]]\nhost = '{PROBE}'\nguest = \"spawn-probe.exe\"\nmode = \"ro\"\n{extra}",
        cmd.join(", ")
    ))
    .unwrap()
}

fn run(s: &JobSpec, results: &std::path::Path) -> Exit {
    let mut env = Account::default().provision(s).unwrap();
    let exit = env.run(s).unwrap();
    env.collect(s, results).unwrap();
    env.destroy().unwrap();
    exit
}

fn alive(pid: &str) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(pid)
}

#[test]
fn account_cannot_read_a_host_file_outside_its_environment() {
    if !set_up() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("antspawn-acct-read-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sentinel = dir.join("outside.txt");
    std::fs::write(&sentinel, b"secret").unwrap();
    let s = spec(&["read", sentinel.to_str().unwrap()], "");
    let exit = run(&s, &dir.join("res"));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        exit,
        Exit::Code(1),
        "the probe must NOT be able to read the sentinel"
    );
}

#[test]
fn account_leaves_nothing_for_the_next_run() {
    if !set_up() {
        return;
    }
    let res = std::env::temp_dir().join(format!("antspawn-acct-slate-{}", std::process::id()));
    assert_eq!(
        run(&spec(&["write", "leftover.txt"], ""), &res),
        Exit::Code(0)
    );
    assert_eq!(
        run(&spec(&["exists", "leftover.txt"], ""), &res),
        Exit::Code(1)
    );
    let _ = std::fs::remove_dir_all(res);
}

#[test]
fn account_kills_a_forked_survivor() {
    if !set_up() {
        return;
    }
    let res = std::env::temp_dir().join(format!("antspawn-acct-fork-{}", std::process::id()));
    let s = spec(
        &["fork", "pid.txt"],
        "[[output]]\nguest = \"pid.txt\"\nname = \"pid\"\n",
    );
    assert_eq!(run(&s, &res), Exit::Code(0));
    let pid = std::fs::read_to_string(res.join("pid")).unwrap();
    let _ = std::fs::remove_dir_all(res);
    assert!(!alive(pid.trim()), "survivor {pid} is still alive");
}
