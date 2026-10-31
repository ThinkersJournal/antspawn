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
use crate::streams::Streams;

/// Present when the optional feature is enabled; the launcher checks it and never enables it.
pub const SANDBOX_EXE: &str = r"C:\Windows\System32\WindowsSandbox.exe";

/// The package's CLI (an app execution alias, so it starts with package identity).
pub fn wsb_exe() -> PathBuf {
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
        require_installed(self.id())?;
        let script = job_script(spec)?;
        let (root, inp, out) = new_root()?;
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

fn require_installed(launcher: &str) -> Result<(), LaunchError> {
    if !Path::new(SANDBOX_EXE).exists() {
        return Err(LaunchError::NotProvisioned {
            launcher: launcher.into(),
            what: format!(
                "{SANDBOX_EXE} not found; enable the Windows Sandbox feature (one-time, \
                 elevated, by the machine owner). This launcher never does that."
            ),
        });
    }
    if !wsb_exe().exists() {
        return Err(LaunchError::NotProvisioned {
            launcher: launcher.into(),
            what: format!(
                "{} not found; the Windows Sandbox app (package MicrosoftWindows.WindowsSandbox) \
                 must be installed. This launcher never installs it.",
                wsb_exe().display()
            ),
        });
    }
    Ok(())
}

/// A fresh host folder with `in` and `out` under it, returned as `(root, in, out)`.
fn new_root() -> Result<(PathBuf, PathBuf, PathBuf), LaunchError> {
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
    Ok((root, inp, out))
}

/// One runner VM (board 260): networking on, the pinned runner mapped read-only, `start` as its command.
/// What reaches the runner's JIT config is the caller's `started` hook, called with the sandbox id; this
/// type holds no secret and never sees one.
pub struct RunnerVm(WsbEnv);

impl RunnerVm {
    pub fn provision(runner_dir: &Path, start: &str) -> Result<Self, LaunchError> {
        require_installed("windows-sandbox")?;
        let script = runner_job_script(start)?;
        let (root, inp, out) = new_root()?;
        let env = WsbEnv {
            root,
            config: runner_wsb_config(&inp, &out, runner_dir),
            out,
            id: None,
            wsb: None,
        };
        std::fs::write(inp.join("job.cmd"), script)?;
        Ok(RunnerVm(env))
    }

    /// Start the VM, call `started(sandbox id)`, wait for the runner to exit or `limit` to pass.
    pub fn run(
        &mut self,
        limit: Option<Duration>,
        started: &mut dyn FnMut(&str) -> Result<(), LaunchError>,
    ) -> Result<Exit, LaunchError> {
        self.0.run_hooked(limit, started)
    }

    /// Copy the guest's `trace.log` and `cmd.log` into `results`.
    pub fn collect_logs(&mut self, results: &Path) -> Result<(), LaunchError> {
        let spec = JobSpec::from_toml("[job]\nname = \"runner\"\ncmd = [\"runner\"]\n")
            .expect("static spec");
        Environment::collect(&mut self.0, &spec, results)
    }

    pub fn destroy(self) -> Result<(), LaunchError> {
        Box::new(self.0).destroy()
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

impl WsbEnv {
    /// Start the VM and the job, call `started` with the sandbox id once `wsb exec` is running (the runner
    /// hand-off uses it to reach the guest), then wait for `exit.code`.
    fn run_hooked(
        &mut self,
        limit: Option<Duration>,
        started: &mut dyn FnMut(&str) -> Result<(), LaunchError>,
    ) -> Result<Exit, LaunchError> {
        reap_orphans();
        let begun = Command::new(wsb_exe())
            .args(["start", "-c", &self.config])
            .output()?;
        let said = String::from_utf8_lossy(&begun.stdout).into_owned();
        let Some(id) = sandbox_id_in(&said) else {
            return Err(LaunchError::Other(format!(
                "wsb start gave no sandbox id (exit {}): {said} {}",
                begun.status,
                String::from_utf8_lossy(&begun.stderr)
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
        if let Err(e) = started(&id) {
            self.close();
            return Err(e);
        }
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
}

impl Environment for WsbEnv {
    fn run(&mut self, spec: &JobSpec) -> Result<Exit, LaunchError> {
        self.run_hooked(spec.limits.time_s.map(Duration::from_secs), &mut |_| Ok(()))
    }

    fn streams(&mut self) -> Streams {
        // The guest redirects the command's stdout and stderr into one file (`2>&1`).
        Streams {
            stdout: std::fs::read(self.out.join("cmd.log"))
                .ok()
                .map(|b| crate::streams::tail_of(&b)),
            stderr: None,
            combined: true,
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

/// Ids of the sandboxes `wsb list` shows now (any owner).
pub fn running_sandboxes() -> Vec<String> {
    Command::new(wsb_exe())
        .arg("list")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
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

/// The guest folder the pinned runner is mapped read-only at; the script copies it to `GUEST_RUNNER` so the
/// runner can write its `_diag` and `_work` (a read-only `_diag` stopped it in the 0.0.32 measurement).
pub const GUEST_RUNNER_SRC: &str = r"C:\antspawn\runner";
const GUEST_RUNNER: &str = r"C:\runner";

/// The configuration for a runner VM (board 260): the one place the runner gets network. Same as
/// [`wsb_config`] with `Networking` enabled and no job mounts, plus the unpacked runner mapped read-only.
/// Nothing of the host except `in`, `out` and the runner is visible.
pub fn runner_wsb_config(inp: &Path, out: &Path, runner_dir: &Path) -> String {
    let spec =
        JobSpec::from_toml("[job]\nname = \"runner\"\ncmd = [\"runner\"]\n").expect("static spec");
    let base = wsb_config(&spec, inp, out).replace(
        "<Networking>Disable</Networking>",
        "<Networking>Enable</Networking>",
    );
    let folder = format!(
        "    <MappedFolder>\n      <HostFolder>{}</HostFolder>\n      \
         <SandboxFolder>{GUEST_RUNNER_SRC}</SandboxFolder>\n      <ReadOnly>true</ReadOnly>\n    </MappedFolder>\n",
        xml(&runner_dir.display().to_string())
    );
    base.replace("  </MappedFolders>", &format!("{folder}  </MappedFolders>"))
}

/// The `job.cmd` of a runner VM: Smart App Control off (guest only), copy the runner out of the read-only
/// mapping, then `start` (the line that starts the runner, whose JIT-config transport is decided by board
/// item 322), then `exit.code`. The JIT config is never part of this text: `start` is a plain command line,
/// and `plain` plus the metacharacter check refuse anything that is not.
pub fn runner_job_script(start: &str) -> Result<String, LaunchError> {
    plain("runner start line", start)?;
    if start.contains(['&', '|', '<', '>', '^']) {
        return Err(LaunchError::Other(
            "the runner start line has a cmd.exe metacharacter".into(),
        ));
    }
    let out = GUEST_OUT;
    let mut s = format!(
        "@echo off\r\nset \"OUT={out}\"\r\n>> \"{out}\\trace.log\" echo [start] %DATE% %TIME%\r\n"
    );
    s += GUEST_SAC_OFF;
    s += &format!(
        "xcopy \"{GUEST_RUNNER_SRC}\" \"{GUEST_RUNNER}\\\" /E /I /Q /Y >nul 2>> \"{out}\\trace.log\"\r\n\
         cd /d \"{GUEST_RUNNER}\"\r\n\
         {start} >> \"{out}\\cmd.log\" 2>&1\r\nset RC=%ERRORLEVEL%\r\n\
         >> \"{out}\\trace.log\" echo [command-exit] %RC%\r\n>\"{out}\\exit.code\" echo %RC%\r\n"
    );
    Ok(s)
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
fn job_script(spec: &JobSpec) -> Result<String, LaunchError> {
    job_script_with(spec, GUEST_OUT, false, GUEST_SAC_OFF)
}

/// Switches Smart App Control off INSIDE the guest, as its first act (board 260, PM ruling A,
/// 2026-10-24). The Windows Sandbox image enforces `VerifiedAndReputableDesktop`, which blocks the
/// GitHub runner's unsigned binaries (`Runner.Listener.dll`: 0x800711C7) and any unsigned exe after
/// its first start. The VM is disposable and the hypervisor, not Smart App Control, is what keeps a
/// job off the host. These commands must never run on the host: this text exists only in the `job.cmd`
/// that `job_script` writes into the mapped `in` folder and `wsb exec` runs in the guest; every
/// host-side use of the script builder passes no prelude (`job_script_in`), and a test counts the uses.
const GUEST_SAC_OFF: &str = concat!(
    "set \"CIKEY=HKLM\\SYSTEM\\CurrentControlSet\\Control\\CI\\Policy\"\r\n",
    ">> \"%OUT%\\trace.log\" echo [sac-before]\r\n",
    "reg query \"%CIKEY%\" /v VerifiedAndReputablePolicyState >> \"%OUT%\\trace.log\" 2>&1\r\n",
    // The guest's own evaluation can rewrite the state while it is still settling after boot
    // (measured: a single write was lost once in four fresh VMs), so write until it reads 0x0.
    "set SACTRIES=0\r\n",
    ":sacloop\r\n",
    "set /a SACTRIES+=1\r\n",
    "reg add \"%CIKEY%\" /v VerifiedAndReputablePolicyState /t REG_DWORD /d 0 /f >nul 2>&1\r\n",
    "CiTool.exe --refresh < nul >nul 2>&1\r\n",
    "reg query \"%CIKEY%\" /v VerifiedAndReputablePolicyState | findstr /c:\"0x0\" >nul 2>&1\r\n",
    "if not errorlevel 1 goto sacdone\r\n",
    "if %SACTRIES% lss 8 (ping -n 3 127.0.0.1 >nul & goto sacloop)\r\n",
    ">> \"%OUT%\\trace.log\" echo [sac-failed after %SACTRIES% tries]\r\n",
    ":sacdone\r\n",
    ">> \"%OUT%\\trace.log\" echo [sac-after tries=%SACTRIES%]\r\n",
    "reg query \"%CIKEY%\" /v VerifiedAndReputablePolicyState >> \"%OUT%\\trace.log\" 2>&1\r\n",
);

/// `job_script` with the output folder and the final `shutdown` as parameters, so a test can run
/// the very same script on the host (a temp folder, no shutdown).
///
/// `trace.log` records the guest's progress and `cmd.log` the command's own output, so a run that
/// never finishes still says how far it got. Redirections are written `>file echo ...`: in
/// `echo %RC%> file` a one-digit `RC` is read as a handle number (`0>`), the file stays empty and
/// the host waits for an exit code that never comes (measured 2026-10-24, decision 253).
pub fn job_script_in(spec: &JobSpec, out: &str, shutdown: bool) -> Result<String, LaunchError> {
    job_script_with(spec, out, shutdown, "")
}

fn job_script_with(
    spec: &JobSpec,
    out: &str,
    shutdown: bool,
    prelude: &str,
) -> Result<String, LaunchError> {
    let mut s = format!(
        "@echo off\r\nset \"OUT={out}\"\r\n>> \"{out}\\trace.log\" echo [start] %DATE% %TIME%\r\n"
    );
    s += prelude;
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
    fn only_the_guest_script_switches_smart_app_control_off() {
        let s = spec("");
        let guest = job_script(&s).unwrap();
        assert!(guest.contains("VerifiedAndReputablePolicyState /t REG_DWORD /d 0"));
        assert!(guest.contains("CiTool.exe --refresh"));
        // The script builder the host-run tests use passes no prelude.
        let host = job_script_in(&s, "C:\\h\\out", false).unwrap();
        assert!(
            !host.contains("VerifiedAndReputable") && !host.contains("CiTool"),
            "{host}"
        );
        // The switch is named in exactly one use: `job_script`. (This file's own text, minus this test.)
        let src = include_str!("windows_sandbox.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert_eq!(
            code.matches("GUEST_SAC_OFF").count(),
            3,
            "the definition and two uses: `job_script` and `runner_job_script`, both guest-only"
        );
        assert_eq!(
            code.matches("job_script_with(").count(),
            3,
            "two callers and the definition"
        );
        // `job_script` is private and only `provision` calls it; `job.cmd` is only ever run by `wsb exec`.
        assert_eq!(code.matches("job_script(spec)").count(), 1);
        assert!(!code.contains("pub fn job_script("));
    }

    #[test]
    fn runner_config_has_network_and_exactly_in_out_and_the_runner() {
        let c = runner_wsb_config(
            Path::new("C:/h/in"),
            Path::new("C:/h/out"),
            Path::new("C:/h/runner"),
        );
        assert!(c.contains("<Networking>Enable</Networking>"));
        assert!(!c.contains("<Networking>Disable</Networking>"));
        assert!(c.contains("<VGpu>Disable</VGpu>"));
        assert_eq!(c.matches("<MappedFolder>").count(), 3);
        assert!(c.contains(GUEST_RUNNER_SRC) && c.contains("C:/h/runner"));
        // Control: the job config of the same builders still has no network.
        let job = wsb_config(&spec(""), Path::new("C:/h/in"), Path::new("C:/h/out"));
        assert!(job.contains("<Networking>Disable</Networking>"));
    }

    #[test]
    fn runner_script_copies_the_runner_then_starts_it_then_writes_exit_code_last() {
        let t = runner_job_script(r"C:\antspawn\in\shim.exe run.cmd").unwrap();
        let (sac, copy, start, code) = (
            t.find("VerifiedAndReputablePolicyState /t REG_DWORD")
                .unwrap(),
            t.find("xcopy").unwrap(),
            t.find("shim.exe").unwrap(),
            t.find("exit.code").unwrap(),
        );
        assert!(sac < copy && copy < start && start < code);
        assert!(runner_job_script("a & b").is_err());
        assert!(runner_job_script("echo %JIT%").is_err());
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
