// SPDX-License-Identifier: MIT OR Apache-2.0
//! The runner and the `account` launcher's file handling, exercised WITHOUT the restricted
//! account: `spawn-runner serve` runs as the current user against a throwaway root. This proves
//! the parts that do not depend on who the runner runs as (Job Object kill of the whole tree,
//! work dir wipe, timeout, copy-in/copy-out, refusals). It does NOT prove account isolation:
//! that is the runner-only test in `tests/account.rs`, UNVERIFIED until the account exists.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;

use antspawn::launchers::Account;
use antspawn::runner::{Layout, decode_exit};
use antspawn::{Exit, JobSpec, Launcher};

const PROBE: &str = env!("CARGO_BIN_EXE_spawn-probe");
const RUNNER: &str = env!("CARGO_BIN_EXE_spawn-runner");

struct Root(PathBuf);

impl Root {
    fn new(tag: &str) -> Root {
        let r = std::env::temp_dir().join(format!("antspawn-rt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        for d in ["spool", "results", "work"] {
            std::fs::create_dir_all(r.join(d)).unwrap();
        }
        std::fs::write(
            r.join("runner.toml"),
            "max_time_s = 30\nmax_ram_mb = 1024\n",
        )
        .unwrap();
        Root(r)
    }
    fn layout(&self) -> Layout {
        Layout::new(&self.0)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn spec(args: &[&str], extra: &str) -> JobSpec {
    let cmd: Vec<String> = std::iter::once("spawn-probe.exe")
        .chain(args.iter().copied())
        .map(|a| format!("'{a}'"))
        .collect();
    JobSpec::from_toml(&format!(
        "[job]\nname = \"p\"\ncmd = [{}]\n[[mount]]\nhost = '{PROBE}'\nguest = \"spawn-probe.exe\"\nmode = \"ro\"\n{extra}",
        cmd.join(", ")
    ))
    .unwrap()
}

/// provision -> `spawn-runner serve` as this user -> read the answer -> collect -> destroy.
fn run_job(root: &Root, spec: &JobSpec, results: &Path) -> Exit {
    let launcher = Account::new(root.layout());
    let mut env = launcher.provision(spec).unwrap();
    let st = Command::new(RUNNER)
        .arg("serve")
        .env("ANTSPAWN_ROOT", &root.0)
        .status()
        .unwrap();
    assert!(st.success());
    let id = std::fs::read_dir(root.0.join("results"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    let code = std::fs::read_to_string(root.0.join("results").join(&id).join("exit.code"))
        .expect("exit.code is published");
    env.collect(spec, results).unwrap();
    env.destroy().unwrap();
    decode_exit(&code).unwrap()
}

fn alive(pid: &str) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(pid)
}

#[test]
fn exit_code_and_output_come_back_and_the_job_dirs_are_gone() {
    let root = Root::new("ok");
    let res = root.0.join("host-results");
    let s = spec(
        &["write", "made.txt"],
        "[[output]]\nguest = \"made.txt\"\nname = \"made\"\n",
    );
    assert_eq!(run_job(&root, &s, &res), Exit::Code(0));
    assert_eq!(std::fs::read(res.join("made")).unwrap(), b"x");
    for d in ["spool", "results", "work"] {
        assert_eq!(
            std::fs::read_dir(root.0.join(d)).unwrap().count(),
            0,
            "{d} must be empty after destroy"
        );
    }
}

#[test]
fn a_forked_survivor_is_killed_before_the_answer_is_published() {
    let root = Root::new("fork");
    let res = root.0.join("host-results");
    let s = spec(
        &["fork", "pid.txt"],
        "[[output]]\nguest = \"pid.txt\"\nname = \"pid\"\n",
    );
    assert_eq!(run_job(&root, &s, &res), Exit::Code(0));
    let pid = std::fs::read_to_string(res.join("pid")).unwrap();
    assert!(!alive(pid.trim()), "survivor {pid} still alive");
}

#[test]
fn a_file_left_by_one_job_is_not_there_for_the_next() {
    let root = Root::new("slate");
    let res = root.0.join("host-results");
    assert_eq!(
        run_job(&root, &spec(&["write", "leftover.txt"], ""), &res),
        Exit::Code(0)
    );
    assert_eq!(
        run_job(&root, &spec(&["exists", "leftover.txt"], ""), &res),
        Exit::Code(10),
        "exit 10 = the probe did not find the file"
    );
}

#[test]
fn the_time_limit_stops_the_job() {
    let root = Root::new("time");
    let res = root.0.join("host-results");
    let s = spec(&["sleep", "20"], "[limits]\ntime_s = 1\n");
    assert_eq!(run_job(&root, &s, &res), Exit::TimedOut);
}

#[test]
fn a_spec_above_the_runner_ceiling_is_refused_before_anything_is_spooled() {
    let root = Root::new("refuse");
    let s = spec(&["sleep", "1"], "[limits]\ntime_s = 31\n");
    let e = Account::new(root.layout()).provision(&s).err().unwrap();
    assert!(e.to_string().contains("time_s"), "{e}");
    assert_eq!(std::fs::read_dir(root.0.join("spool")).unwrap().count(), 0);
}

#[test]
fn a_missing_setup_is_reported_as_not_provisioned() {
    let nowhere = Layout::new(std::env::temp_dir().join("antspawn-no-such-root"));
    let e = Account::new(nowhere)
        .provision(&spec(&["sleep", "1"], ""))
        .err()
        .unwrap();
    assert!(
        matches!(e, antspawn::LaunchError::NotProvisioned { .. }),
        "{e}"
    );
    assert!(e.to_string().contains("account-setup.ps1"));
}
