// SPDX-License-Identifier: MIT OR Apache-2.0
//! The negative controls. Each `violates_*` function runs a `spawn-probe` job through a launcher and
//! reports whether the isolation property was BROKEN. A certified launcher must make all of them
//! `false`; the `naive` launcher must make all of them `true`, which proves the probes can fail.
//! Shared by `tests/negative.rs` and `examples/matrix.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Exit, JobSpec, Launcher};

/// A probe binary and the launcher run it is used for.
pub struct Probes {
    exe: PathBuf,
}

impl Probes {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Probes { exe: exe.into() }
    }

    fn spec(&self, args: &[&str]) -> JobSpec {
        let cmd: Vec<String> = std::iter::once(self.exe.to_str().expect("utf-8 probe path"))
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
    fn succeeds(&self, l: &dyn Launcher, args: &[&str]) -> bool {
        let s = self.spec(args);
        let mut env = l.provision(&s).unwrap();
        let exit = env.run(&s).unwrap();
        env.destroy().unwrap();
        exit == Exit::Code(0)
    }

    /// Can a job read a host file outside its environment?
    pub fn violates_read(&self, l: &dyn Launcher, sentinel: &Path) -> bool {
        self.succeeds(l, &["read", sentinel.to_str().unwrap()])
    }

    /// Can a job create a host file outside its environment? (The file is removed afterwards.)
    pub fn violates_write(&self, l: &dyn Launcher, target: &Path) -> bool {
        let wrote = self.succeeds(l, &["write", target.to_str().unwrap()]);
        let _ = std::fs::remove_file(target);
        wrote
    }

    /// Does a file written by one job exist for the next one?
    pub fn violates_clean_slate(&self, l: &dyn Launcher) -> bool {
        assert!(
            self.succeeds(l, &["write", "leftover.txt"]),
            "first job must be able to write"
        );
        self.succeeds(l, &["exists", "leftover.txt"])
    }

    /// Is a process the job forked still alive after the environment is destroyed?
    pub fn violates_kill_runaway(&self, l: &dyn Launcher, pidfile: &Path) -> bool {
        assert!(self.succeeds(l, &["fork", pidfile.to_str().unwrap()]));
        let pid = std::fs::read_to_string(pidfile).unwrap();
        let alive = alive(pid.trim());
        kill(pid.trim());
        alive
    }
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
