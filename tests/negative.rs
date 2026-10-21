// SPDX-License-Identifier: MIT OR Apache-2.0
//! The negative controls. Each `violates_*` function runs a probe job through a launcher and
//! reports whether the isolation property was BROKEN. A certified launcher must make all three
//! `false`; the `naive` launcher must make all three `true`, which proves the probes can fail.
//! Launchers that need privileged setup run these in runner-only tests (UNVERIFIED in CI).

use std::path::{Path, PathBuf};
use std::process::Command;

use antspawn::launchers::Naive;
use antspawn::{Exit, JobSpec, Launcher};

const PROBE: &str = env!("CARGO_BIN_EXE_spawn-probe");

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("antspawn-test-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spec(args: &[&str]) -> JobSpec {
    let cmd: Vec<String> = std::iter::once(PROBE)
        .chain(args.iter().copied())
        .map(|a| format!("'{a}'"))
        .collect();
    JobSpec::from_toml(&format!(
        "[job]\nname = \"probe\"\ncmd = [{}]\n[limits]\ntime_s = 30\n",
        cmd.join(", ")
    ))
    .unwrap()
}

/// Run one probe in a fresh environment of `l`, then destroy it. `true` = exit code 0.
fn succeeds(l: &dyn Launcher, args: &[&str]) -> bool {
    let s = spec(args);
    let mut env = l.provision(&s).unwrap();
    let exit = env.run(&s).unwrap();
    env.destroy().unwrap();
    exit == Exit::Code(0)
}

/// Can a job read a host file outside its environment?
pub fn violates_read(l: &dyn Launcher, sentinel: &Path) -> bool {
    succeeds(l, &["read", sentinel.to_str().unwrap()])
}

/// Does a file written by one job exist for the next one?
pub fn violates_clean_slate(l: &dyn Launcher) -> bool {
    assert!(
        succeeds(l, &["write", "leftover.txt"]),
        "first job must be able to write"
    );
    succeeds(l, &["exists", "leftover.txt"])
}

/// Is a process the job forked still alive after the environment is destroyed?
pub fn violates_kill_runaway(l: &dyn Launcher, pidfile: &Path) -> bool {
    assert!(succeeds(l, &["fork", pidfile.to_str().unwrap()]));
    let pid = std::fs::read_to_string(pidfile).unwrap();
    let alive = alive(pid.trim());
    kill(pid.trim());
    alive
}

fn alive(pid: &str) -> bool {
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(pid)
    } else {
        Command::new("kill")
            .args(["-0", pid])
            .status()
            .unwrap()
            .success()
    }
}

fn kill(pid: &str) {
    let _ = if cfg!(windows) {
        Command::new("taskkill").args(["/F", "/PID", pid]).output()
    } else {
        Command::new("kill").args(["-9", pid]).output()
    };
}

#[test]
fn naive_fails_every_negative_control() {
    let dir = scratch("naive");
    let sentinel = dir.join("outside.txt");
    std::fs::write(&sentinel, b"secret").unwrap();
    let _ = std::fs::remove_dir_all(Naive::work_dir());

    assert!(violates_read(&Naive, &sentinel), "naive must read outside");
    assert!(
        violates_clean_slate(&Naive),
        "naive must leave state behind"
    );
    assert!(
        violates_kill_runaway(&Naive, &dir.join("pid")),
        "naive must leave the forked process alive"
    );

    let _ = std::fs::remove_dir_all(Naive::work_dir());
    let _ = std::fs::remove_dir_all(dir);
}
