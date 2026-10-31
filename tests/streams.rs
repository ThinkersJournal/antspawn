// SPDX-License-Identifier: MIT OR Apache-2.0
//! The job's stdout and stderr tails come back from `Environment::streams`, and a job's own exit
//! code 2 stays a job exit (never confused with a refusal). Run against `naive`, the launcher every
//! CI leg has; the other launchers' streams are runner-only.

use antspawn::launchers::Naive;
use antspawn::{Exit, JobSpec, Launcher};

fn spec(script: &str) -> JobSpec {
    let cmd = if cfg!(windows) {
        format!("[\"cmd\", \"/c\", {script:?}]")
    } else {
        format!("[\"sh\", \"-c\", {script:?}]")
    };
    JobSpec::from_toml(&format!("[job]\nname = \"s\"\ncmd = {cmd}\n")).unwrap()
}

#[test]
fn stdout_and_stderr_tails_come_back_and_exit_2_stays_a_job_exit() {
    let script = if cfg!(windows) {
        "echo to-out& echo to-err 1>&2& exit 2"
    } else {
        "echo to-out; echo to-err >&2; exit 2"
    };
    let s = spec(script);
    let mut env = Naive.provision(&s).unwrap();
    assert_eq!(env.run(&s).unwrap(), Exit::Code(2));
    let streams = env.streams();
    let out = String::from_utf8_lossy(streams.stdout.as_deref().unwrap());
    let err = String::from_utf8_lossy(streams.stderr.as_deref().unwrap());
    assert!(out.contains("to-out") && !out.contains("to-err"), "{out:?}");
    assert!(err.contains("to-err") && !err.contains("to-out"), "{err:?}");
    assert!(!streams.combined);
}

#[test]
fn control_a_silent_job_has_empty_tails_not_missing_ones() {
    let s = spec("exit 0");
    let mut env = Naive.provision(&s).unwrap();
    assert_eq!(env.run(&s).unwrap(), Exit::Code(0));
    let streams = env.streams();
    assert_eq!(streams.stdout.as_deref(), Some(&b""[..]));
    assert_eq!(streams.stderr.as_deref(), Some(&b""[..]));
}
