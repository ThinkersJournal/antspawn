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

    /// Can a job open a TCP connection to `addr`? `None` when the host itself cannot (no network
    /// here), because then a refused job proves nothing: the control must connect first.
    pub fn violates_network(&self, l: &dyn Launcher, addr: &str) -> Option<bool> {
        if !host_can_connect(addr) {
            return None;
        }
        Some(self.succeeds(l, &["connect", addr]))
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

/// An address a job must not reach when the network is off: a public IP, so the loopback exemption
/// in the host firewall cannot hide a hole.
pub const NET_ADDR: &str = "1.1.1.1:443";

/// The positive control for the network probe: can this process itself connect to `addr`?
pub fn host_can_connect(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    addr.to_socket_addrs().is_ok_and(|mut it| {
        it.any(|a| {
            std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_secs(5)).is_ok()
        })
    })
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

/// The same probes through the `account` launcher, which cannot bind host paths: the probe binary is
/// copied in as a read-only mount and outputs are copied out. Shared by `tests/account.rs` (asserts
/// nothing is violated) and `examples/matrix.rs` (records the rows). Windows only.
#[cfg(windows)]
pub mod account {
    use std::path::{Path, PathBuf};

    use crate::launchers::Account;
    use crate::{Exit, JobSpec, Launcher};

    use super::{alive, host_can_connect, kill};

    /// Has the one-time elevated setup been applied here (is the task registered)?
    pub fn set_up() -> bool {
        std::process::Command::new("schtasks")
            .args(["/Query", "/TN", crate::runner::TASK_NAME])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antspawn-acct-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch dir");
        d
    }

    fn spec(probe: &Path, args: &[&str], extra: &str) -> JobSpec {
        let cmd: Vec<String> = std::iter::once("spawn-probe.exe")
            .chain(args.iter().copied())
            .map(|a| format!("'{a}'"))
            .collect();
        JobSpec::from_toml(&format!(
            "[job]\nname = \"p\"\ncmd = [{}]\n[limits]\ntime_s = 30\n[[mount]]\nhost = '{}'\nguest = \"spawn-probe.exe\"\nmode = \"ro\"\n{extra}",
            cmd.join(", "),
            probe.display()
        ))
        .unwrap()
    }

    fn run(s: &JobSpec, results: &Path) -> Exit {
        let mut env = Account::default().provision(s).expect("provision");
        let exit = env.run(s).expect("run");
        env.collect(s, results).expect("collect");
        env.destroy().expect("destroy");
        exit
    }

    /// The probe's own answer: `true` = the action succeeded. Any other exit (unknown arguments,
    /// killed, timed out) is inconclusive and panics, so it can never read as "isolated".
    fn succeeded(s: &JobSpec, results: &Path) -> bool {
        match run(s, results) {
            Exit::Code(0) => true,
            Exit::Code(10) => false,
            other => panic!("probe was inconclusive: {other:?}"),
        }
    }

    pub fn violates_read(probe: &Path) -> bool {
        let d = scratch("read");
        let sentinel = d.join("outside.txt");
        std::fs::write(&sentinel, b"secret").unwrap();
        let s = spec(probe, &["read", sentinel.to_str().unwrap()], "");
        let v = succeeded(&s, &d.join("res"));
        let _ = std::fs::remove_dir_all(d);
        v
    }

    pub fn violates_write(probe: &Path) -> bool {
        let d = scratch("write");
        let target = d.join("written.txt");
        let s = spec(probe, &["write", target.to_str().unwrap()], "");
        let ran = succeeded(&s, &d.join("res"));
        let v = ran || target.exists();
        let _ = std::fs::remove_dir_all(d);
        v
    }

    pub fn violates_clean_slate(probe: &Path) -> bool {
        let d = scratch("slate");
        assert!(
            succeeded(&spec(probe, &["write", "leftover.txt"], ""), &d),
            "first job must be able to write"
        );
        let v = succeeded(&spec(probe, &["exists", "leftover.txt"], ""), &d);
        let _ = std::fs::remove_dir_all(d);
        v
    }

    pub fn violates_kill_runaway(probe: &Path) -> bool {
        let d = scratch("fork");
        let s = spec(
            probe,
            &["fork", "pid.txt"],
            "[[output]]\nguest = \"pid.txt\"\nname = \"pid\"\n",
        );
        // `succeeded` also collects: the pid file lands in `d/pid`.
        assert!(succeeded(&s, &d), "the fork probe must run");
        let pid = std::fs::read_to_string(d.join("pid")).unwrap();
        let v = alive(pid.trim());
        kill(pid.trim());
        let _ = std::fs::remove_dir_all(d);
        v
    }

    /// `None` when this host cannot reach `addr` itself (no control, no verdict).
    pub fn violates_network(probe: &Path, addr: &str) -> Option<bool> {
        if !host_can_connect(addr) {
            return None;
        }
        let d = scratch("net");
        let v = succeeded(&spec(probe, &["connect", addr], ""), &d);
        let _ = std::fs::remove_dir_all(d);
        Some(v)
    }
}

/// The same probes through `windows-sandbox`. The VM has its own disk, so a host path the job names
/// does not exist inside it: "read" and "write" are violated only if the HOST sees the effect, and
/// "kills runaway" means the VM (and with it everything the job forked) is gone after `destroy`. The
/// probes reach the guest as a read-only mapped folder holding a `cmd` script. Shared by `tests/windows_sandbox.rs`
/// and `examples/matrix.rs`. Windows only.
#[cfg(windows)]
pub mod sandbox {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use crate::launchers::WindowsSandbox;
    use crate::launchers::windows_sandbox::{SANDBOX_EXE, running_sandboxes, wsb_exe};
    use crate::{Exit, JobSpec, Launcher};

    use super::host_can_connect;

    /// Is the Windows Sandbox feature on and `wsb.exe` installed here?
    pub fn available() -> bool {
        Path::new(SANDBOX_EXE).exists() && wsb_exe().exists()
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antspawn-sbx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch dir");
        d
    }

    /// Exit 0 = yes, 10 = no, 3 = the tool the probe relies on does not work here (so "no" would
    /// prove nothing). Written as a script of Windows' own signed tools: the guest runs Smart App
    /// Control, which blocks an unsigned executable such as `spawn-probe.exe` (measured 2026-10-24:
    /// exit 0xC0000... "blocked by your organization's Device Guard policy", intermittently).
    pub const PROBE_CMD: &str = "@echo off\r\n\
        if /i \"%~1\"==\"read\" goto read\r\n\
        if /i \"%~1\"==\"write\" goto write\r\n\
        if /i \"%~1\"==\"exists\" goto exists\r\n\
        if /i \"%~1\"==\"connect\" goto connect\r\n\
        exit /b 2\r\n\
        :read\r\n\
        type \"%SystemRoot%\\System32\\drivers\\etc\\hosts\" >nul 2>&1\r\n\
        if errorlevel 1 exit /b 3\r\n\
        type \"%~2\" >nul 2>&1\r\n\
        goto answer\r\n\
        :write\r\n\
        copy nul \"%TEMP%\\antspawn-control.txt\" >nul 2>&1\r\n\
        if errorlevel 1 exit /b 3\r\n\
        copy nul \"%~2\" >nul 2>&1\r\n\
        goto answer\r\n\
        :exists\r\n\
        if exist \"%~2\" exit /b 0\r\n\
        exit /b 10\r\n\
        :connect\r\n\
        curl.exe --version >nul 2>&1\r\n\
        if errorlevel 1 exit /b 3\r\n\
        curl.exe -sS -k --connect-timeout 5 -o NUL \"https://%~2/\" >nul 2>&1\r\n\
        goto answer\r\n\
        :answer\r\n\
        if errorlevel 1 exit /b 10\r\n\
        exit /b 0\r\n";

    /// A folder holding `probe.cmd`, because only folders can be mapped.
    fn staged(d: &Path) -> PathBuf {
        let dir = d.join("probe");
        std::fs::create_dir_all(&dir).expect("probe dir");
        std::fs::write(dir.join("probe.cmd"), PROBE_CMD).expect("stage probe");
        dir
    }

    fn spec(probe_dir: &Path, args: &[&str], extra: &str) -> JobSpec {
        let cmd: Vec<String> = ["cmd", "/c", "C:\\probe\\probe.cmd"]
            .into_iter()
            .chain(args.iter().copied())
            .map(|a| format!("'{a}'"))
            .collect();
        JobSpec::from_toml(&format!(
            "[job]\nname = \"p\"\ncmd = [{}]\n[limits]\ntime_s = 120\n[[mount]]\nhost = '{}'\nguest = 'C:\\probe'\nmode = \"ro\"\n{extra}",
            cmd.join(", "),
            probe_dir.display()
        ))
        .unwrap()
    }

    fn run(s: &JobSpec, results: &Path) -> Exit {
        let mut env = WindowsSandbox.provision(s).expect("provision");
        let exit = env.run(s).expect("run");
        env.collect(s, results).expect("collect");
        env.destroy().expect("destroy");
        exit
    }

    /// The probe's own answer: `true` = the action succeeded. Any other exit is inconclusive and
    /// panics, so it can never read as "isolated".
    fn succeeded(s: &JobSpec, results: &Path) -> bool {
        match run(s, results) {
            Exit::Code(0) => true,
            Exit::Code(10) => false,
            other => panic!("probe was inconclusive: {other:?}"),
        }
    }

    pub fn violates_read(_probe: &Path) -> bool {
        let d = scratch("read");
        let sentinel = d.join("outside.txt");
        std::fs::write(&sentinel, b"secret").unwrap();
        let s = spec(&staged(&d), &["read", sentinel.to_str().unwrap()], "");
        let v = succeeded(&s, &d.join("res"));
        let _ = std::fs::remove_dir_all(d);
        v
    }

    pub fn violates_write(_probe: &Path) -> bool {
        let d = scratch("write");
        let target = d.join("written.txt");
        let s = spec(&staged(&d), &["write", target.to_str().unwrap()], "");
        let _ = succeeded(&s, &d.join("res"));
        let v = target.exists();
        let _ = std::fs::remove_dir_all(d);
        v
    }

    pub fn violates_clean_slate(_probe: &Path) -> bool {
        let d = scratch("slate");
        let dir = staged(&d);
        assert!(
            succeeded(&spec(&dir, &["write", "leftover.txt"], ""), &d),
            "first job must be able to write"
        );
        let v = succeeded(&spec(&dir, &["exists", "leftover.txt"], ""), &d);
        let _ = std::fs::remove_dir_all(d);
        v
    }

    /// The job forks a survivor, then the environment is destroyed: is any sandbox still running?
    pub fn violates_kill_runaway(_probe: &Path) -> bool {
        let d = scratch("fork");
        // Not `spawn-probe fork`: the guest's code-integrity policy lets an unsigned file start once, so
        // the probe could not start its own sleeper (measured 2026-10-24). A signed `ping` stands in.
        let s = JobSpec::from_toml(
            "[job]
name = \"fork\"
cmd = ['powershell', '-NoProfile', '-Command', \"Start-Process ping -ArgumentList '-n','300','127.0.0.1'\"]
[limits]
time_s = 120
",
        )
        .unwrap();
        let exit = run(&s, &d);
        assert_eq!(exit, Exit::Code(0), "the job that forks must run");
        let start = Instant::now();
        let mut v = !running_sandboxes().is_empty();
        while v && start.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(500));
            v = !running_sandboxes().is_empty();
        }
        let _ = std::fs::remove_dir_all(d);
        v
    }

    /// `None` when this host cannot reach `addr` itself (no control, no verdict).
    pub fn violates_network(_probe: &Path, addr: &str) -> Option<bool> {
        if !host_can_connect(addr) {
            return None;
        }
        let d = scratch("net");
        let v = succeeded(&spec(&staged(&d), &["connect", addr], ""), &d);
        let _ = std::fs::remove_dir_all(d);
        Some(v)
    }
}
