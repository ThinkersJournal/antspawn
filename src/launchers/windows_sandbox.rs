// SPDX-License-Identifier: MIT OR Apache-2.0
//! `windows-sandbox`: one throwaway Windows Sandbox VM per job.
//!
//! Windows Sandbox is an optional Windows feature that CireSnave enables once, with elevation;
//! this launcher never enables it and says so loudly when `WindowsSandbox.exe` is absent.
//!
//! Flow: `wsb start` with a configuration (networking and vGPU disabled, mapped folders), `wsb exec`
//! of the generated `job.cmd` as System, poll the mapped `out` folder for `exit.code`, then
//! `wsb stop` and remove the host folders. The job runs through `job.cmd` so no command line goes
//! through XML. `wsb.exe` is the package's own CLI and starts with package identity; the
//! System32 `WindowsSandbox.exe <file>.wsb` stub starts the client without it, and the client then
//! crashes loading `WinRT.Runtime` (measured 2026-10-23, board 253). A `LogonCommand` never ran
//! under `wsb start`, so `exec` is how the job is started.
//! Declared outputs are copied by that script into `out` before it writes `exit.code`.
//!
//! Capabilities are certified only by running the negative tests on a machine that has the
//! feature; until a matrix row says `yes` this launcher grants nothing (UNVERIFIED).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::launcher::{Environment, Exit, LaunchError, Launcher};
use crate::spec::{JobSpec, MountMode};

/// Present when the optional feature is enabled; the launcher checks it and never enables it.
pub const SANDBOX_EXE: &str = r"C:\Windows\System32\WindowsSandbox.exe";

/// The package's CLI (an app execution alias, so it starts with package identity).
fn wsb_exe() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(r"Microsoft\WindowsApps\wsb.exe")
}
const GUEST_IN: &str = r"C:\antspawn\in";
const GUEST_OUT: &str = r"C:\antspawn\out";

/// The Windows Sandbox launcher.
#[derive(Debug, Default)]
pub struct WindowsSandbox;

impl Launcher for WindowsSandbox {
    fn id(&self) -> &str {
        "windows-sandbox"
    }

    fn provision(&self, spec: &JobSpec) -> Result<Box<dyn Environment>, LaunchError> {
        if !Path::new(SANDBOX_EXE).exists() {
            return Err(LaunchError::NotProvisioned {
                launcher: self.id().into(),
                what: format!(
                    "{SANDBOX_EXE} not found; enable the Windows Sandbox feature (one-time, \
                     elevated, by the machine owner). This launcher never does that."
                ),
            });
        }
        if !wsb_exe().exists() {
            return Err(LaunchError::NotProvisioned {
                launcher: self.id().into(),
                what: format!(
                    "{} not found; the Windows Sandbox app (package MicrosoftWindows.WindowsSandbox) \
                     must be installed. This launcher never installs it.",
                    wsb_exe().display()
                ),
            });
        }
        let script = job_script(spec)?;
        let root = std::env::temp_dir().join(format!(
            "antspawn-wsb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let (inp, out) = (root.join("in"), root.join("out"));
        std::fs::create_dir_all(&inp)?;
        std::fs::create_dir_all(&out)?;
        // Everything below is under `root`; the guard removes it on any early return or drop.
        let env = WsbEnv {
            root,
            config: wsb_config(spec, &inp, &out),
            out,
            id: None,
            wsb: None,
        };
        std::fs::write(inp.join("job.cmd"), script)?;
        Ok(Box::new(env))
    }
}

struct WsbEnv {
    root: PathBuf,
    out: PathBuf,
    config: String,
    /// The sandbox id `wsb start` printed; set once the VM exists.
    id: Option<String>,
    /// The running `wsb exec`, which lasts as long as the job.
    wsb: Option<std::process::Child>,
}

impl WsbEnv {
    fn close(&mut self) {
        if let Some(mut c) = self.wsb.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        // Only this job's own sandbox, by id: never another lane's.
        if let Some(id) = self.id.take() {
            let _ = Command::new(wsb_exe()).args(["stop", "--id", &id]).output();
            let _ = std::fs::remove_file(owner_file(&id));
        }
    }
}

impl Environment for WsbEnv {
    fn run(&mut self, spec: &JobSpec) -> Result<Exit, LaunchError> {
        reap_orphans();
        let started = Command::new(wsb_exe())
            .args(["start", "-c", &self.config])
            .output()?;
        let said = String::from_utf8_lossy(&started.stdout).into_owned();
        let Some(id) = sandbox_id_in(&said) else {
            return Err(LaunchError::Other(format!(
                "wsb start gave no sandbox id (exit {}): {said} {}",
                started.status,
                String::from_utf8_lossy(&started.stderr)
            )));
        };
        self.id = Some(id.clone());
        // Written before anything else can fail, so a run that is killed outright is still found.
        let _ = std::fs::write(owner_file(&id), std::process::id().to_string());
        self.wsb = Some(
            Command::new(wsb_exe())
                .args(["exec", "--id", &id, "-r", "System", "-c"])
                .arg(format!("cmd.exe /c {GUEST_IN}\\job.cmd"))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?,
        );
        let limit = spec.limits.time_s.map(Duration::from_secs);
        let start = Instant::now();
        let code_file = self.out.join("exit.code");
        let mut last_crash_check = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(&code_file) {
                // The script writes the file in one go, but be safe about a partial read.
                if let Ok(code) = text.trim().parse::<i32>() {
                    return Ok(Exit::Code(code));
                }
            }
            // `wsb exec` ends when the job does; it ending without `exit.code` means the VM went away.
            if let Some(Ok(Some(st))) = self.wsb.as_mut().map(std::process::Child::try_wait)
                && !code_file.exists()
            {
                return Err(LaunchError::Other(format!(
                    "wsb exec exited with {st} before the job wrote exit.code \
                     (the VM did not run the job or was closed)"
                )));
            }
            // The client crashes within a second of starting on some hosts (board 253); the log says
            // so, and waiting out the limit would only hide it.
            if last_crash_check.elapsed() >= Duration::from_secs(2) {
                last_crash_check = Instant::now();
                if let Some(event) = client_crash_since(start.elapsed() + Duration::from_secs(2)) {
                    self.close();
                    return Err(LaunchError::Other(format!(
                        "the Windows Sandbox client crashed after the VM was requested (Application log): {event}"
                    )));
                }
            }
            if limit.is_some_and(|l| start.elapsed() >= l) {
                self.close();
                return Ok(Exit::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn collect(&mut self, spec: &JobSpec, results: &Path) -> Result<(), LaunchError> {
        std::fs::create_dir_all(results)?;
        for o in &spec.outputs {
            let from = self.out.join(&o.name);
            if from.exists() {
                copy(&from, &results.join(&o.name))?;
            }
        }
        // The guest's own account of the run, kept even when the job never finished.
        for log in ["trace.log", "cmd.log"] {
            if self.out.join(log).is_file() {
                std::fs::copy(
                    self.out.join(log),
                    results.join(format!("windows-sandbox.{log}")),
                )?;
            }
        }
        Ok(())
    }

    fn destroy(mut self: Box<Self>) -> Result<(), LaunchError> {
        self.close();
        remove_dir_retrying(&self.root, Duration::from_secs(60)).map_err(Into::into)
    }
}

impl Drop for WsbEnv {
    fn drop(&mut self) {
        self.close();
        let _ = remove_dir_retrying(&self.root, Duration::from_secs(10));
    }
}

/// Records which process owns sandbox `id`, so a later run can stop one whose owner was killed
/// (no `Drop` runs then).
fn owner_file(id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("antspawn-wsb-{id}.owner"))
}

fn process_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
}

/// Stops the sandboxes this launcher started whose owning process is gone. Only ids recorded by
/// `owner_file` are touched, never another tool's sandbox.
fn reap_orphans() {
    let Ok(dir) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name
            .strip_prefix("antspawn-wsb-")
            .and_then(|n| n.strip_suffix(".owner"))
        else {
            continue;
        };
        let owner = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok());
        if owner.is_some_and(process_alive) {
            continue;
        }
        let _ = Command::new(wsb_exe()).args(["stop", "--id", id]).output();
        let _ = std::fs::remove_file(entry.path());
    }
}

/// The sandbox id in `wsb start` output (`Id: <guid>`).
fn sandbox_id_in(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Id:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
}

/// The first `Application Error` / `.NET Runtime` event of a Windows Sandbox executable logged in
/// the last `age`, as one line, or `None`. Read with `wevtutil` (about 50 ms, no PowerShell).
fn client_crash_since(age: Duration) -> Option<String> {
    let query = format!(
        "*[System[(Provider[@Name='Application Error'] or Provider[@Name='.NET Runtime'])          and TimeCreated[timediff(@SystemTime) <= {}]]]",
        age.as_millis()
    );
    let out = Command::new("wevtutil")
        .args(["qe", "Application", "/f:text", "/c:20", "/rd:true"])
        .arg(format!("/q:{query}"))
        .output()
        .ok()?;
    sandbox_crash_in(&String::from_utf8_lossy(&out.stdout))
}

/// One line for the first event in `wevtutil /f:text` output that names a Windows Sandbox
/// executable together with a crash.
fn sandbox_crash_in(text: &str) -> Option<String> {
    text.split("Event[")
        .filter(|e| {
            e.contains("WindowsSandbox")
                && (e.contains("Faulting application") || e.contains("unhandled exception"))
        })
        .map(|e| {
            let pick = |key: &str| {
                e.lines()
                    .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
            };
            let mut parts = vec![];
            for key in [
                "Faulting application name:",
                "Faulting package full name:",
                "Exception Info:",
            ] {
                if let Some(v) = pick(key) {
                    parts.push(format!("{key} {v}"));
                }
            }
            parts.join("; ")
        })
        .next()
}

/// The VM keeps its mapped folders open for a while after its client is killed (measured
/// 2026-10-23: `remove_dir_all` failed with a sharing violation, os error 32), so retry until
/// `patience` runs out and then report the last error.
fn remove_dir_retrying(dir: &Path, patience: Duration) -> std::io::Result<()> {
    let start = Instant::now();
    loop {
        match std::fs::remove_dir_all(dir) {
            Err(e) if !matches!(e.kind(), std::io::ErrorKind::NotFound) => {
                if start.elapsed() >= patience {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            _ => return Ok(()),
        }
    }
}

fn copy(from: &Path, to: &Path) -> Result<(), LaunchError> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The `wsb start` configuration: no network, no vGPU, the `in` folder read-only, `out` writable, plus the
/// spec's `ro`/`rw` mounts.
pub fn wsb_config(spec: &JobSpec, inp: &Path, out: &Path) -> String {
    let folder = |host: &Path, guest: &str, ro: bool| {
        format!(
            "    <MappedFolder>\n      <HostFolder>{}</HostFolder>\n      \
             <SandboxFolder>{}</SandboxFolder>\n      <ReadOnly>{ro}</ReadOnly>\n    </MappedFolder>\n",
            xml(&host.display().to_string()),
            xml(guest)
        )
    };
    let mut folders = folder(inp, GUEST_IN, true) + &folder(out, GUEST_OUT, false);
    for m in &spec.mounts {
        if let (MountMode::Ro | MountMode::Rw, Some(host)) = (m.mode, &m.host) {
            folders += &folder(host, &m.guest, m.mode == MountMode::Ro);
        }
    }
    format!(
        "<Configuration>\n  <VGpu>Disable</VGpu>\n  <Networking>Disable</Networking>\n  \
         <ClipboardRedirection>Disable</ClipboardRedirection>\n  \
         <PrinterRedirection>Disable</PrinterRedirection>\n  \
         <AudioInput>Disable</AudioInput>\n  <VideoInput>Disable</VideoInput>\n  \
         <ProtectedClient>Enable</ProtectedClient>\n  <MappedFolders>\n{folders}  </MappedFolders>\n\
         </Configuration>\n"
    )
}

fn plain(what: &str, s: &str) -> Result<(), LaunchError> {
    if s.contains(['"', '%', '\r', '\n', '\0']) {
        return Err(LaunchError::Other(format!(
            "{what} contains a character this launcher cannot pass safely through cmd.exe \
             (one of \" % or a line break)"
        )));
    }
    Ok(())
}

/// The batch file the VM runs at logon: environment, scratch dirs, the command, output copies,
/// then `exit.code` last, so its presence means everything else is already in `out`.
pub fn job_script(spec: &JobSpec) -> Result<String, LaunchError> {
    job_script_in(spec, GUEST_OUT, false)
}

/// `job_script` with the output folder and the final `shutdown` as parameters, so a test can run
/// the very same script on the host (a temp folder, no shutdown).
///
/// `trace.log` records the guest's progress and `cmd.log` the command's own output, so a run that
/// never finishes still says how far it got. Redirections are written `>file echo ...`: in
/// `echo %RC%> file` a one-digit `RC` is read as a handle number (`0>`), the file stays empty and
/// the host waits for an exit code that never comes (measured 2026-10-24, decision 253).
pub fn job_script_in(spec: &JobSpec, out: &str, shutdown: bool) -> Result<String, LaunchError> {
    let mut s = format!("@echo off\r\n>> \"{out}\\trace.log\" echo [start] %DATE% %TIME%\r\n");
    for (k, v) in &spec.job.env {
        plain("env name", k)?;
        plain("env value", v)?;
        if k.contains(['=', '&', '|', '<', '>', '^']) || v.contains(['&', '|', '<', '>', '^']) {
            return Err(LaunchError::Other(format!(
                "env `{k}` has a cmd.exe metacharacter"
            )));
        }
        s += &format!("set \"{k}={v}\"\r\n");
    }
    for m in spec.mounts.iter().filter(|m| m.mode == MountMode::Scratch) {
        plain("scratch path", &m.guest)?;
        s += &format!("mkdir \"{}\"\r\n", m.guest);
    }
    if let Some(cwd) = &spec.job.cwd {
        plain("cwd", cwd)?;
        s += &format!("cd /d \"{cwd}\"\r\n");
    }
    let mut parts = Vec::new();
    for a in &spec.job.cmd {
        plain("command argument", a)?;
        if a.contains(['&', '|', '<', '>', '^']) {
            return Err(LaunchError::Other(format!(
                "argument `{a}` has a cmd.exe metacharacter"
            )));
        }
        parts.push(format!("\"{a}\""));
    }
    s += &format!(
        "{} >> \"{out}\\cmd.log\" 2>&1\r\nset RC=%ERRORLEVEL%\r\n>> \"{out}\\trace.log\" echo [command-exit] %RC%\r\n",
        parts.join(" ")
    );
    for o in &spec.outputs {
        plain("output path", &o.guest)?;
        plain("output name", &o.name)?;
        s += &format!(
            "if exist \"{g}\\*\" (xcopy \"{g}\" \"{out}\\{n}\\\" /E /I /Q /Y >nul) else \
             (copy \"{g}\" \"{out}\\{n}\" >nul)\r\n",
            g = o.guest,
            n = o.name
        );
    }
    s += &format!(">\"{out}\\exit.code\" echo %RC%\r\n");
    if shutdown {
        s += "shutdown /s /t 0\r\n";
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(extra: &str) -> JobSpec {
        JobSpec::from_toml(&format!(
            "[job]\nname = \"t\"\ncmd = [\"slangc\", \"-v\"]\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn config_disables_network_and_maps_declared_folders_only() {
        let s = spec(
            "[[mount]]\nhost = \"C:/src\"\nguest = \"C:\\\\work\"\nmode = \"ro\"\n\
             [[mount]]\nguest = \"C:\\\\tmp\"\nmode = \"scratch\"\n",
        );
        let c = wsb_config(&s, Path::new("C:/h/in"), Path::new("C:/h/out"));
        assert!(c.contains("<Networking>Disable</Networking>"));
        assert!(c.contains("<VGpu>Disable</VGpu>"));
        assert_eq!(
            c.matches("<MappedFolder>").count(),
            3,
            "in, out, one ro mount"
        );
        assert!(c.contains("<ReadOnly>true</ReadOnly>"));
        assert!(
            !c.contains("C:\\tmp"),
            "scratch is guest-only, never mapped"
        );
    }

    #[test]
    fn script_writes_exit_code_last() {
        let s = spec("[[output]]\nguest = \"C:\\\\work\\\\o.txt\"\nname = \"o\"\n");
        let t = job_script(&s).unwrap();
        let (cmd, copy, code) = (
            t.find("\"slangc\" \"-v\"").unwrap(),
            t.find("copy").unwrap(),
            t.find("exit.code").unwrap(),
        );
        assert!(cmd < copy && copy < code);
    }

    #[test]
    fn cmd_metacharacters_are_refused_not_escaped() {
        let s = JobSpec::from_toml("[job]\nname = \"t\"\ncmd = [\"a\", \"b & calc\"]\n").unwrap();
        assert!(job_script(&s).is_err());
        let s = JobSpec::from_toml("[job]\nname = \"t\"\ncmd = [\"a\", \"%PATH%\"]\n").unwrap();
        assert!(job_script(&s).is_err());
    }

    /// Run the generated script on the host (output folder in a temp dir, no shutdown) and return
    /// the files it left in that folder.
    #[cfg(windows)]
    fn run_script_on_host(tag: &str, script: impl FnOnce(&str) -> String) -> PathBuf {
        let out =
            std::env::temp_dir().join(format!("antspawn-script-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let bat =
            std::env::temp_dir().join(format!("antspawn-script-{tag}-{}.cmd", std::process::id()));
        std::fs::write(&bat, script(&out.display().to_string())).unwrap();
        let st = Command::new("cmd").arg("/c").arg(&bat).status().unwrap();
        assert!(st.success() || st.code().is_some(), "cmd ran: {st}");
        let _ = std::fs::remove_file(&bat);
        out
    }

    #[cfg(windows)]
    #[test]
    fn exit_code_file_is_written_for_every_exit_code_including_one_digit() {
        // Arguments are quoted in the script, so pick programs with a known code and no switches:
        // `hostname` exits 0 and `findstr` with no arguments exits 2 (the one-digit cases
        // `echo %RC%> file` got wrong); a missing program exits 9009.
        for (prog, want) in [
            ("hostname", 0),
            ("findstr", 2),
            ("antspawn-no-such-exe", 9009),
        ] {
            let s =
                JobSpec::from_toml(&format!("[job]\nname = \"t\"\ncmd = [\"{prog}\"]\n")).unwrap();
            let out = run_script_on_host(&format!("rc{want}"), |o| {
                job_script_in(&s, o, false).unwrap()
            });
            let text = std::fs::read_to_string(out.join("exit.code")).unwrap();
            assert_eq!(text.trim().parse::<i32>().ok(), Some(want), "{text:?}");
            let trace = std::fs::read_to_string(out.join("trace.log")).unwrap();
            assert!(trace.contains("[start]") && trace.contains(&format!("[command-exit] {want}")));
            let _ = std::fs::remove_dir_all(out);
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_old_redirect_form_leaves_an_empty_file_for_a_one_digit_code() {
        // Known-bad control: the form the launcher used before 0.0.23. If this ever passes, the
        // test above no longer proves anything about the one-digit case.
        let out = run_script_on_host("old", |o| {
            format!("@echo off\r\nset RC=0\r\necho %RC%> \"{o}\\exit.code\"\r\n")
        });
        let text = std::fs::read_to_string(out.join("exit.code")).unwrap();
        assert!(text.trim().parse::<i32>().is_err(), "{text:?}");
        let _ = std::fs::remove_dir_all(out);
    }

    #[cfg(windows)]
    #[test]
    fn a_command_that_cannot_start_leaves_its_reason_in_cmd_log() {
        let s =
            JobSpec::from_toml("[job]\nname = \"t\"\ncmd = [\"antspawn-no-such-exe\"]\n").unwrap();
        let out = run_script_on_host("nope", |o| job_script_in(&s, o, false).unwrap());
        let log = std::fs::read_to_string(out.join("cmd.log")).unwrap();
        assert!(log.contains("antspawn-no-such-exe"), "{log:?}");
        let code = std::fs::read_to_string(out.join("exit.code")).unwrap();
        assert_eq!(code.trim(), "9009");
        let _ = std::fs::remove_dir_all(out);
    }

    #[cfg(windows)]
    #[test]
    fn removal_waits_for_a_held_file_and_gives_up_on_a_stuck_one() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("antspawn-retry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let held = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .share_mode(0)
            .open(dir.join("f"))
            .unwrap();
        // Control: while the file is held, a short patience reports the sharing violation.
        let e = remove_dir_retrying(&dir, Duration::from_millis(600)).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(32), "{e}");
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1200));
            drop(held);
        });
        remove_dir_retrying(&dir, Duration::from_secs(20)).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn missing_feature_is_reported_not_repaired() {
        if !Path::new(SANDBOX_EXE).exists() {
            let e = WindowsSandbox.provision(&spec("")).err().unwrap();
            assert!(matches!(e, LaunchError::NotProvisioned { .. }), "{e}");
        }
    }

    #[test]
    fn the_sandbox_id_is_read_from_wsb_start_output_and_nothing_else_is_taken_for_one() {
        let ok = "Windows Sandbox environment started successfully:\nId: baae564b-bf6d-4e6b-8e9d-cefaafcd0575\n";
        assert_eq!(
            sandbox_id_in(ok).as_deref(),
            Some("baae564b-bf6d-4e6b-8e9d-cefaafcd0575")
        );
        assert_eq!(sandbox_id_in("Error: Id: not a guid!"), None);
        assert_eq!(sandbox_id_in("Id:\n"), None);
        assert_eq!(sandbox_id_in(""), None);
    }

    #[test]
    fn a_sandbox_client_crash_event_is_recognised_and_other_crashes_are_not() {
        let crash = "Event[0]:
  Log Name: Application
  Description:
  Faulting application name: WindowsSandboxRemoteSession.exe, version: 0.8.107.0
  Faulting package full name: 
";
        let line = sandbox_crash_in(crash).expect("crash recognised");
        assert!(line.contains("WindowsSandboxRemoteSession.exe"), "{line}");
        let other = "Event[0]:
  Description:
  Faulting application name: notepad.exe, version: 1
";
        assert_eq!(sandbox_crash_in(other), None);
        assert_eq!(sandbox_crash_in(""), None);
    }
}
