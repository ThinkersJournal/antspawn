// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-probe`: the tiny jobs the negative tests run inside a launcher's environment. It is
//! test support, never part of a job. Exit 0 means the action succeeded.
//!
//! * `read <path>`: exit 0 if the file can be read.
//! * `write <path>`: exit 0 if the file can be created.
//! * `exists <path>`: exit 0 if the path exists.
//! * `fork <pidfile>`: start a detached 60 s sleeper, write its pid to `pidfile`, exit.
//! * `sleep <secs>`: sleep.

use std::process::{Command, ExitCode, Stdio};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ok = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["read", p] => std::fs::read(p).is_ok(),
        ["write", p] => std::fs::write(p, b"x").is_ok(),
        ["exists", p] => std::path::Path::new(p).exists(),
        ["sleep", s] => {
            std::thread::sleep(std::time::Duration::from_secs(s.parse().unwrap_or(0)));
            true
        }
        ["fork", pidfile] => {
            let exe = std::env::current_exe().expect("own path");
            match Command::new(exe)
                .args(["sleep", "60"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => std::fs::write(pidfile, child.id().to_string()).is_ok(),
                Err(_) => false,
            }
        }
        _ => {
            eprintln!("spawn-probe: unknown arguments {args:?}");
            return ExitCode::from(2);
        }
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
