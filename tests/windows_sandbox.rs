// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `windows-sandbox` launcher against the REAL Windows Sandbox: runner-only. Isolation is
//! proven by a probe script run INSIDE the VM, and the same script run on the host is the
//! known-bad control (it must report the leaks the VM does not have), so a pass could have failed.
//!
//! Where the feature or `wsb.exe` is missing the tests print `UNVERIFIED` and return; they are never
//! `#[ignore]`d. Record real runs in `docs/MEASUREMENTS.md` with date and ref.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use antspawn::launcher::{Exit, Launcher};
use antspawn::launchers::{WindowsSandbox, windows_sandbox::SANDBOX_EXE};
use antspawn::spec::JobSpec;

/// One sandbox at a time: the host's concurrency limit is unmeasured.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn wsb() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
        .join(r"Microsoft\WindowsApps\wsb.exe")
}

fn available() -> bool {
    let ok = Path::new(SANDBOX_EXE).exists() && wsb().exists();
    if !ok {
        println!("UNVERIFIED: Windows Sandbox (feature + wsb.exe) is not available here");
    }
    ok
}

fn listed() -> Vec<String> {
    let out = Command::new(wsb()).arg("list").output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn wait_until(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < limit, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The probe: one `name=value` line per check. `blocked`/`ok` are the good answers, `LEAK` and
/// `BROKEN` the bad ones. `%1` is the results file, `%2` the host profile directory.
fn probe_script() -> String {
    let mut s = String::from("@echo off\r\n>\"%~1\" echo start\r\n");
    let check = |name: &str, cmd: &str, good_if_fails: bool| {
        let (fail, pass) = if good_if_fails {
            ("blocked", "LEAK")
        } else {
            ("BROKEN", "ok")
        };
        format!(
            "{cmd} >nul 2>&1\r\nif errorlevel 1 (>>\"%~1\" echo {name}={fail}) else (>>\"%~1\" echo {name}={pass})\r\n"
        )
    };
    s += &check("read_projects", "dir C:\\Projects", true);
    s += &check("read_profile", "dir \"%~2\"", true);
    s += &check("control_read_windows", "dir C:\\Windows", false);
    // `copy nul` has no redirection of its own, so the check's `>nul` cannot swallow the write.
    s += &check("write_ro_input", "copy nul \"%~dp0w.txt\"", true);
    s += &check("control_write_temp", "copy nul \"%TEMP%\\c.txt\"", false);
    s += &check("net", "ping -n 1 -w 3000 1.1.1.1", true);
    s
}

fn spec(extra: &str, probe_dir: &Path, results: &str, profile: &str) -> JobSpec {
    JobSpec::from_toml(&format!(
        "[job]\nname = \"probe\"\ncmd = ['cmd', '/c', 'C:\\probe\\probe.cmd', '{results}', '{profile}']\n\
         [limits]\ntime_s = 240\n\
         [[mount]]\nhost = '{}'\nguest = 'C:\\probe'\nmode = \"ro\"\n\
         [[output]]\nguest = '{results}'\nname = \"res.txt\"\n{extra}",
        probe_dir.display()
    ))
    .unwrap()
}

fn results_of(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("res.txt")).unwrap_or_default()
}

fn profile() -> String {
    std::env::var("USERPROFILE").unwrap()
}

fn staged(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("antspawn-wsbtest-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("probe.cmd"), probe_script()).unwrap();
    dir
}

#[test]
fn the_sandbox_has_no_host_files_network_or_writable_input_and_leaves_nothing() {
    if !available() {
        return;
    }
    let _g = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = staged("iso");
    let results = dir.join("results");

    // Known-bad control: on the host the same script finds the leaks.
    let host_res = dir.join("host-res.txt");
    let st = Command::new("cmd")
        .arg("/c")
        .arg(dir.join("probe.cmd"))
        .arg(&host_res)
        .arg(profile())
        .status()
        .unwrap();
    assert!(st.code().is_some(), "{st}");
    let host = std::fs::read_to_string(&host_res).unwrap();
    assert!(
        host.contains("read_profile=LEAK") && host.contains("control_read_windows=ok"),
        "the probe must fail on the host, or it proves nothing: {host}"
    );
    let host_net = host.contains("net=LEAK");
    if !host_net {
        println!("UNVERIFIED: the host cannot ping 1.1.1.1, so there is no network control");
    }

    let l = WindowsSandbox;
    let s = spec("", &dir, "C:\\res.txt", &profile());
    let mut env = l.provision(&s).unwrap();
    let exit = env.run(&s).unwrap();
    // Control for "nothing left": while the job's VM exists it is listed.
    assert!(
        !listed().is_empty(),
        "no sandbox listed while one is running"
    );
    env.collect(&s, &results).unwrap();
    env.destroy().unwrap();
    let guest = results_of(&results);
    println!("GUEST probe:\n{guest}");
    assert!(matches!(exit, Exit::Code(_)), "{exit:?}");
    for line in [
        "control_read_windows=ok",
        "control_write_temp=ok",
        "read_projects=blocked",
        "read_profile=blocked",
        "write_ro_input=blocked",
    ] {
        assert!(guest.lines().any(|l| l.trim() == line), "{line}: {guest}");
    }
    if host_net {
        assert!(
            guest.lines().any(|l| l.trim() == "net=blocked"),
            "network is on inside the sandbox: {guest}"
        );
    }
    wait_until("the sandbox to be gone", Duration::from_secs(60), || {
        listed().is_empty()
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_killed_run_leaves_a_sandbox_that_the_next_run_stops() {
    if std::env::var_os("ANTSPAWN_WSB_CHILD").is_some() {
        // Child mode: start a long job and never return (the parent kills this process).
        if available() {
            let dir = staged("child");
            let s = spec("", &dir, "C:\\res.txt", &profile());
            let _ = (|| {
                let mut env = WindowsSandbox.provision(&s).ok()?;
                env.run(&JobSpec::from_toml(
                    "[job]\nname = \"long\"\ncmd = ['ping', '-n', '300', '127.0.0.1']\n[limits]\ntime_s = 280\n",
                ).ok()?).ok()
            })();
        }
        return;
    }
    if !available() {
        return;
    }
    let _g = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        listed().is_empty(),
        "a sandbox is already running: {:?}",
        listed()
    );
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "a_killed_run_leaves_a_sandbox_that_the_next_run_stops",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("ANTSPAWN_WSB_CHILD", "1")
        .spawn()
        .unwrap();
    wait_until("the child's sandbox", Duration::from_secs(120), || {
        !listed().is_empty()
    });
    let orphan = listed();
    child.kill().unwrap();
    let _ = child.wait();
    std::thread::sleep(Duration::from_secs(2));
    // Control: a killed owner really does leave its VM behind.
    assert_eq!(
        listed(),
        orphan,
        "the kill did not leave the VM; the test proves nothing"
    );

    // The next run reaps it: provision + run a trivial job, then nothing is left.
    let dir = staged("reap");
    let s = spec("", &dir, "C:\\res.txt", &profile());
    let mut env = WindowsSandbox.provision(&s).unwrap();
    env.run(&s).unwrap();
    let during = listed();
    assert!(
        orphan.iter().all(|id| !during.contains(id)),
        "the orphan {orphan:?} survived the next run: {during:?}"
    );
    env.destroy().unwrap();
    wait_until("no sandbox left", Duration::from_secs(60), || {
        listed().is_empty()
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// The probes the matrix records, asserted here too: a pass is a pass that could have failed (the
/// `naive` launcher fails the same probes in `tests/negative.rs`).
#[test]
fn the_matrix_probes_find_the_sandbox_isolated() {
    use antspawn::probes::{NET_ADDR, sandbox};
    if !available() {
        return;
    }
    let _g = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let probe = Path::new(env!("CARGO_BIN_EXE_spawn-probe"));
    assert!(
        !sandbox::violates_read(probe),
        "the probe read the sentinel"
    );
    assert!(!sandbox::violates_write(probe), "the host saw a file");
    assert!(!sandbox::violates_clean_slate(probe));
    assert!(!sandbox::violates_kill_runaway(probe), "a sandbox survived");
    match sandbox::violates_network(probe, NET_ADDR) {
        Some(v) => assert!(!v, "the probe connected to {NET_ADDR}"),
        None => println!("UNVERIFIED: this host cannot reach {NET_ADDR}, so there is no control"),
    }
}

/// Known-bad control for the matrix probes: the SAME script, run on the host (no isolation), must
/// answer "yes, the action succeeded" (exit 0) for each, so a "no" from the VM means something.
#[test]
fn the_matrix_probe_script_finds_every_leak_on_the_host() {
    use antspawn::probes::{NET_ADDR, sandbox::PROBE_CMD};
    let dir = std::env::temp_dir().join(format!("antspawn-hostctl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("probe.cmd");
    std::fs::write(&script, PROBE_CMD).unwrap();
    let sentinel = dir.join("outside.txt");
    std::fs::write(&sentinel, b"secret").unwrap();
    let written = dir.join("written.txt");
    let code = |args: &[&str]| {
        Command::new("cmd")
            .arg("/c")
            .arg(&script)
            .args(args)
            .status()
            .unwrap()
            .code()
    };
    assert_eq!(code(&["read", sentinel.to_str().unwrap()]), Some(0));
    assert_eq!(code(&["write", written.to_str().unwrap()]), Some(0));
    assert!(written.exists());
    assert_eq!(code(&["exists", sentinel.to_str().unwrap()]), Some(0));
    assert_eq!(
        code(&["read", dir.join("nope").to_str().unwrap()]),
        Some(10)
    );
    if antspawn::probes::host_can_connect(NET_ADDR) {
        assert_eq!(code(&["connect", NET_ADDR]), Some(0));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
