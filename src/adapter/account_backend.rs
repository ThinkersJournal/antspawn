// SPDX-License-Identifier: MIT OR Apache-2.0
//! The account-backed runner path (ROADMAP row 21, `docs/ACCOUNT-DISPATCH.md`, board 341): a runner job
//! on a per-runner restricted local account instead of in a Windows Sandbox VM.
//!
//! Two pieces, both behind the same seams `SandboxDispatch` already uses:
//! * [`AccountBackend`] is a [`RunnerBackend`]: it starts the account's scheduled task (which runs
//!   `spawn-jit-shim --pipe` as that account), calls `started` with the pipe name so the transport can
//!   deliver, then waits for the task to end. The task itself is behind [`AccountTask`], so tests use a
//!   fake; [`SchtasksTask`] is the real one.
//! * [`PipeTransport`] is a [`JitTransport`]: it connects to `\\.\pipe\<name>` as the launcher user,
//!   retrying until the shim has created it, writes the config once and closes. Whoever else is not on the
//!   pipe's access list is refused by the system; a connection is never retried once made (the shim
//!   takes exactly one).
//!
//! **Not run live.** Nothing here has been run against a real account, task or Task Scheduler: that needs
//! the elevated provisioning (CireSnave's). The `schtasks /Query` output format is unmeasured (English
//! `Status:` line assumed, anything else is an error), as is whether a non-admin `schtasks /Run` can
//! trigger the task. Errors name the pipe and the I/O kind, never the config.

use std::process::Command;
use std::time::{Duration, Instant};

use super::dispatch::{JitTransport, RunnerBackend};
use super::github::{RunnerGroup, Secret};

/// The scheduled task that runs the shim as the runner account.
pub trait AccountTask: Send + Sync + 'static {
    /// Trigger the task. Returns once it has been asked to run.
    fn start(&self) -> Result<(), String>;
    /// Ask the task to stop (best effort; used when delivery failed).
    fn end(&self);
    /// Wait until the task is no longer running, at most `limit`; `Ok` is a one-line account.
    fn wait(&self, limit: Duration) -> Result<String, String>;
}

/// One runner on one account: trigger its task, hand the config over its pipe, wait for the end.
pub struct AccountBackend<T: AccountTask> {
    pub task: T,
    /// Pipe name the shim was told to create (a fixed argument of the registered task).
    pub pipe_name: String,
    /// Longest a runner may live (`AdapterConfig::clamp_time`).
    pub limit: Duration,
}

impl<T: AccountTask> RunnerBackend for AccountBackend<T> {
    fn run_one(
        &self,
        _group: &RunnerGroup,
        started: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<String, String> {
        self.task.start()?;
        if let Err(e) = started(&self.pipe_name) {
            self.task.end();
            return Err(e);
        }
        self.task.wait(self.limit)
    }
}

/// Writes the JIT config once to `\\.\pipe\<id>` (Windows). `window` is how long to wait for the shim to
/// create the pipe after the task starts.
#[derive(Debug, Clone, Copy)]
pub struct PipeTransport {
    pub window: Duration,
}

/// ERROR_FILE_NOT_FOUND (no pipe yet) and ERROR_PIPE_BUSY (the one instance is taken).
const NOT_THERE_YET: i32 = 2;
const PIPE_BUSY: i32 = 231;

impl JitTransport for PipeTransport {
    fn deliver(&self, pipe_name: &str, jit: &Secret) -> Result<(), String> {
        write_once(pipe_name, jit, self.window)
    }
}

#[cfg(windows)]
fn write_once(pipe_name: &str, jit: &Secret, window: Duration) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Write;

    if pipe_name.is_empty()
        || !pipe_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
    {
        return Err("not a pipe name".into());
    }
    let path = format!(r"\\.\pipe\{pipe_name}");
    let deadline = Instant::now() + window;
    let mut pipe = loop {
        match OpenOptions::new().write(true).open(&path) {
            Ok(pipe) => break pipe,
            // Not created yet: the task is still starting. Busy means someone else holds the one
            // instance; waiting cannot help, so that is an error at once.
            Err(e) if e.raw_os_error() == Some(NOT_THERE_YET) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) => {
                return Err(format!("pipe {pipe_name} is already in use"));
            }
            Err(e) => return Err(format!("could not open pipe {pipe_name}: {}", e.kind())),
        }
    };
    pipe.write_all(jit.expose().as_bytes())
        .and_then(|()| pipe.flush())
        .map_err(|e| format!("could not write to pipe {pipe_name}: {}", e.kind()))
}

#[cfg(not(windows))]
fn write_once(_: &str, _: &Secret, _: Duration) -> Result<(), String> {
    let _ = (NOT_THERE_YET, PIPE_BUSY, Instant::now());
    Err("the account path is only available on Windows".into())
}

/// The real [`AccountTask`]: a scheduled task run with `schtasks`. **Unmeasured** (see the module note).
#[derive(Debug, Clone)]
pub struct SchtasksTask {
    pub name: String,
    /// How often to look at the task's status while waiting.
    pub poll: Duration,
}

/// The task's `Status:` from `schtasks /Query /FO LIST /V` output: `Running`, `Ready`, `Disabled`, ...
pub fn parse_task_status(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|l| {
        let (key, value) = l.split_once(':')?;
        (key.trim() == "Status").then(|| value.trim().to_owned())
    })
}

impl SchtasksTask {
    fn query(&self) -> Result<String, String> {
        let out = Command::new("schtasks")
            .args(["/Query", "/TN", &self.name, "/FO", "LIST", "/V"])
            .output()
            .map_err(|e| format!("could not run schtasks: {}", e.kind()))?;
        if !out.status.success() {
            return Err(format!("schtasks /Query failed ({})", out.status));
        }
        parse_task_status(&String::from_utf8_lossy(&out.stdout))
            .ok_or_else(|| "schtasks /Query gave no Status line".to_string())
    }
}

impl AccountTask for SchtasksTask {
    fn start(&self) -> Result<(), String> {
        let out = Command::new("schtasks")
            .args(["/Run", "/TN", &self.name])
            .output()
            .map_err(|e| format!("could not run schtasks: {}", e.kind()))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "`schtasks /Run /TN {}` failed ({}): {}. The task is missing or this user may not run it; setup is the elevated step.",
                self.name,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn end(&self) {
        let _ = Command::new("schtasks")
            .args(["/End", "/TN", &self.name])
            .output();
    }

    fn wait(&self, limit: Duration) -> Result<String, String> {
        let deadline = Instant::now() + limit;
        // A task that was just triggered may still read Ready for a moment: it must be seen Running
        // once, or the grace period must pass, before Ready counts as "ended".
        let grace = Instant::now() + Duration::from_secs(30);
        let mut seen_running = false;
        loop {
            let status = self.query()?;
            if status.eq_ignore_ascii_case("Running") {
                seen_running = true;
            } else if seen_running || Instant::now() >= grace {
                return Ok(format!("task ended ({status})"));
            }
            if Instant::now() >= deadline {
                self.end();
                return Err(format!("task still running after {limit:?}; ended"));
            }
            std::thread::sleep(self.poll);
        }
    }
}

/// One [`AccountBackend`] per group, built from the group's own `task` and `pipe`: the `account` launcher
/// serves several runner accounts, one per group.
#[derive(Debug, Clone, Copy)]
pub struct PerGroupAccountBackend {
    pub limit: Duration,
    pub poll: Duration,
}

impl RunnerBackend for PerGroupAccountBackend {
    fn run_one(
        &self,
        group: &RunnerGroup,
        started: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<String, String> {
        let (Some(task), Some(pipe)) = (&group.task, &group.pipe) else {
            return Err(format!("group `{}` has no task or pipe", group.name));
        };
        AccountBackend {
            task: SchtasksTask {
                name: task.clone(),
                poll: self.poll,
            },
            pipe_name: pipe.clone(),
            limit: self.limit,
        }
        .run_one(group, started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn a_group_without_task_and_pipe_is_refused_before_anything_starts() {
        let b = PerGroupAccountBackend {
            limit: Duration::from_secs(1),
            poll: Duration::from_secs(1),
        };
        let mut g = group();
        g.task = None;
        let e = b
            .run_one(&g, &mut |_| panic!("must not deliver"))
            .unwrap_err();
        assert!(e.contains("`g`"), "{e}");
    }

    /// Records the calls it gets, in order.
    #[derive(Default)]
    struct FakeTask {
        calls: Mutex<Vec<&'static str>>,
        fail_start: bool,
    }
    impl AccountTask for std::sync::Arc<FakeTask> {
        fn start(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("start");
            if self.fail_start {
                Err("no such task".into())
            } else {
                Ok(())
            }
        }
        fn end(&self) {
            self.calls.lock().unwrap().push("end");
        }
        fn wait(&self, _: Duration) -> Result<String, String> {
            self.calls.lock().unwrap().push("wait");
            Ok("task ended (Ready)".into())
        }
    }

    fn group() -> RunnerGroup {
        RunnerGroup {
            name: "g".into(),
            repos: vec!["O/a".into()],
            labels: vec!["antspawn".into()],
            launcher: "account".into(),
            task: Some("t".into()),
            pipe: Some("p".into()),
        }
    }

    fn backend(task: &std::sync::Arc<FakeTask>) -> AccountBackend<std::sync::Arc<FakeTask>> {
        AccountBackend {
            task: task.clone(),
            pipe_name: "ghr-pipe".into(),
            limit: Duration::from_secs(1),
        }
    }

    #[test]
    fn the_task_starts_then_delivery_gets_the_pipe_name_then_the_wait() {
        let task = std::sync::Arc::new(FakeTask::default());
        let mut got = Vec::new();
        let out = backend(&task).run_one(&group(), &mut |id| {
            got.push(id.to_owned());
            Ok(())
        });
        assert_eq!(out, Ok("task ended (Ready)".into()));
        assert_eq!(got, ["ghr-pipe"]);
        assert_eq!(*task.calls.lock().unwrap(), ["start", "wait"]);
    }

    #[test]
    fn a_task_that_will_not_start_delivers_nothing() {
        let task = std::sync::Arc::new(FakeTask {
            fail_start: true,
            ..FakeTask::default()
        });
        let out = backend(&task).run_one(&group(), &mut |_| panic!("no delivery without a task"));
        assert_eq!(out, Err("no such task".into()));
        assert_eq!(*task.calls.lock().unwrap(), ["start"]);
    }

    #[test]
    fn a_failed_delivery_ends_the_task_and_is_the_error() {
        let task = std::sync::Arc::new(FakeTask::default());
        let out = backend(&task).run_one(&group(), &mut |_| Err("pipe busy".into()));
        assert_eq!(out, Err("pipe busy".into()));
        assert_eq!(*task.calls.lock().unwrap(), ["start", "end"]);
    }

    #[test]
    fn task_status_is_read_from_list_output_and_nothing_else() {
        let listing = "Folder: \\\r\nHostName:      PC\r\nTaskName:      \\ghr\r\nStatus:        Running\r\nLogon Mode:    Interactive\r\n";
        assert_eq!(parse_task_status(listing).as_deref(), Some("Running"));
        // Control: output without a Status line yields nothing rather than a guess.
        assert_eq!(parse_task_status("TaskName: x\r\nNext Run Time: N/A"), None);
        assert_eq!(parse_task_status(""), None);
    }

    #[test]
    fn a_pipe_that_never_appears_is_an_error_that_names_no_secret() {
        let t = PipeTransport {
            window: Duration::from_millis(200),
        };
        let err = t
            .deliver("antspawn-no-such-pipe", &Secret::new("c2VjcmV0Cg==".into()))
            .unwrap_err();
        assert!(!err.contains("c2VjcmV0"), "{err}");
    }

    #[test]
    fn a_name_that_is_not_a_pipe_name_is_refused() {
        let t = PipeTransport {
            window: Duration::from_millis(50),
        };
        for bad in ["", r"..\x", "a b"] {
            assert!(
                t.deliver(bad, &Secret::new("YQ==".into())).is_err(),
                "{bad:?}"
            );
        }
    }
}
