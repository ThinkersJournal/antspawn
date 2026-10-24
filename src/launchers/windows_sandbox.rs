// SPDX-License-Identifier: MIT OR Apache-2.0
//! `windows-sandbox`: one throwaway Windows Sandbox VM per job.
//!
//! Windows Sandbox is an optional Windows feature that CireSnave enables once, with elevation;
//! this launcher never enables it and says so loudly when `WindowsSandbox.exe` is absent.
//!
//! Flow: write a `.wsb` (networking and vGPU disabled, mapped folders, a logon command), start
//! it, poll the mapped `out` folder for `exit.code`, then close the VM and remove the host
//! folders. The job runs through a generated `job.cmd` so no command line goes through XML.
//! Declared outputs are copied by that script into `out` before it writes `exit.code`.
//!
//! Capabilities are certified only by running the negative tests on a machine that has the
//! feature; until a matrix row says `yes` this launcher grants nothing (UNVERIFIED).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::launcher::{Environment, Exit, LaunchError, Launcher};
use crate::spec::{JobSpec, MountMode};

pub const SANDBOX_EXE: &str = r"C:\Windows\System32\WindowsSandbox.exe";
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
            out,
            wsb: None,
        };
        std::fs::write(inp.join("job.cmd"), script)?;
        std::fs::write(env.root.join("job.wsb"), wsb_config(spec, &inp, &env.out))?;
        Ok(Box::new(env))
    }
}

struct WsbEnv {
    root: PathBuf,
    out: PathBuf,
    wsb: Option<std::process::Child>,
}

impl WsbEnv {
    fn close(&mut self) {
        if let Some(mut c) = self.wsb.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        // The launcher process may have handed off to the VM client; close that too.
        for image in [
            "WindowsSandboxClient.exe",
            "WindowsSandboxRemoteSession.exe",
        ] {
            let _ = Command::new("taskkill").args(["/F", "/IM", image]).output();
        }
    }
}

impl Environment for WsbEnv {
    fn run(&mut self, spec: &JobSpec) -> Result<Exit, LaunchError> {
        self.wsb = Some(
            Command::new(SANDBOX_EXE)
                .arg(self.root.join("job.wsb"))
                .spawn()?,
        );
        let limit = spec.limits.time_s.map(Duration::from_secs);
        let start = Instant::now();
        let code_file = self.out.join("exit.code");
        loop {
            if let Ok(text) = std::fs::read_to_string(&code_file) {
                // The script writes the file in one go, but be safe about a partial read.
                if let Ok(code) = text.trim().parse::<i32>() {
                    return Ok(Exit::Code(code));
                }
            }
            // A launcher process that fails says so now; waiting out the limit would hide why.
            // (One that exits 0 may just have handed off to the VM client, so that is not checked.)
            if let Some(Ok(Some(st))) = self.wsb.as_mut().map(std::process::Child::try_wait)
                && !st.success()
                && !code_file.exists()
            {
                return Err(LaunchError::Other(format!(
                    "WindowsSandbox.exe exited with {st} before the job wrote exit.code \
                     (the VM did not start or was closed)"
                )));
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

/// The `.wsb` document: no network, no vGPU, the `in` folder read-only, `out` writable, plus the
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
         <ProtectedClient>Enable</ProtectedClient>\n  <MappedFolders>\n{folders}  </MappedFolders>\n  \
         <LogonCommand>\n    <Command>{GUEST_IN}\\job.cmd</Command>\n  </LogonCommand>\n</Configuration>\n"
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
    job_script_in(spec, GUEST_OUT, true)
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
}
