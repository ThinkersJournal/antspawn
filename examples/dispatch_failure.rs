// SPDX-License-Identifier: MIT OR Apache-2.0
//! Measures how a failed account dispatch ENDS (ROADMAP row 24), against the real Task Scheduler and a real
//! runner account. Windows only; run as the launcher user (no elevation), with the account already set up.
//!
//! ```text
//! cargo run --example dispatch_failure -- <task> <pipe>
//! ```
//!
//! Two cases, each through the real `SandboxDispatch` + `AccountBackend` + `SchtasksTask` + `PipeTransport`
//! with a dummy config that is never delivered:
//! * **task missing**: the task name does not exist, so `schtasks /Run` fails (nothing started);
//! * **pipe never appears**: the real task is started but the transport is told a pipe name the shim does not
//!   create, so delivery times out and the backend ends the task.
//!
//! For each it prints the outcome text, `nothing_ran`, how long it took, and what is left behind (the task's
//! status, the pipe listed, `spawn-jit-shim` processes). It reads and starts only the one task named.

use std::time::{Duration, Instant};

use antspawn::adapter::account_backend::{AccountBackend, PipeTransport, SchtasksTask};
use antspawn::adapter::dispatch::SandboxDispatch;
use antspawn::adapter::github::{RunnerGroup, Secret};
use antspawn::adapter::watcher::{Dispatch, QueuedJob};

fn group() -> RunnerGroup {
    RunnerGroup {
        name: "measure".into(),
        repos: vec!["O/a".into()],
        labels: vec!["antspawn".into()],
        launcher: "account".into(),
        task: None,
        pipe: None,
    }
}

fn job() -> QueuedJob {
    QueuedJob {
        repo: "O/a".into(),
        run_id: 1,
        job_id: 1,
        labels: vec![],
    }
}

fn left_behind(task: &str, pipe: &str) -> String {
    let status = std::process::Command::new("schtasks")
        .args(["/Query", "/TN", task, "/FO", "LIST", "/V"])
        .output()
        .ok()
        .and_then(|o| {
            antspawn::adapter::account_backend::parse_task_status(&String::from_utf8_lossy(
                &o.stdout,
            ))
        })
        .unwrap_or_else(|| "no status (task missing?)".into());
    let listed = std::fs::read_dir(r"\\.\pipe\")
        .map(|d| d.flatten().any(|e| e.file_name().to_string_lossy() == pipe))
        .unwrap_or(false);
    let shims = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq spawn-jit-shim.exe", "/NH"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.contains("spawn-jit-shim"))
                .count()
        })
        .unwrap_or(0);
    format!(
        "task status = {status}; pipe {pipe} listed = {listed}; spawn-jit-shim processes = {shims}"
    )
}

fn case(label: &str, task: &str, started_pipe: &str, window: Duration, real_pipe: &str) {
    println!("== {label}");
    println!("before: {}", left_behind(task, real_pipe));
    let mut d = SandboxDispatch::new(
        AccountBackend {
            task: SchtasksTask {
                name: task.to_string(),
                poll: Duration::from_secs(1),
            },
            pipe_name: started_pipe.to_string(),
            limit: Duration::from_secs(60),
        },
        PipeTransport { window },
    );
    let t0 = Instant::now();
    d.dispatch(
        &group(),
        &job(),
        Secret::new("dummy-never-delivered".into()),
    )
    .expect("dispatch returns at once");
    d.join();
    let took = t0.elapsed();
    for o in d.take_outcomes() {
        println!(
            "outcome: result = {:?}; nothing_ran = {}; took {:.1} s",
            o.result,
            o.nothing_ran,
            took.as_secs_f64()
        );
    }
    println!("after (immediately): {}", left_behind(task, real_pipe));
    std::thread::sleep(Duration::from_secs(5));
    println!("after (+5 s): {}", left_behind(task, real_pipe));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(task), Some(pipe)) = (args.first(), args.get(1)) else {
        eprintln!("usage: dispatch_failure <task> <pipe>");
        std::process::exit(2);
    };
    case(
        "task missing",
        "AntSpawnRunner-no-such-task",
        pipe,
        Duration::from_secs(3),
        pipe,
    );
    case(
        "pipe never appears",
        task,
        "antspawn-no-such-pipe",
        Duration::from_secs(3),
        pipe,
    );
}
