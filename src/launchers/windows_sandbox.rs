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

const SANDBOX_EXE: &str = r"C:\Windows\System32\WindowsSandbox.exe";
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
        Ok(())
    }

    fn destroy(mut self: Box<Self>) -> Result<(), LaunchError> {
        self.close();
        std::fs::remove_dir_all(&self.root).map_err(Into::into)
    }
}

impl Drop for WsbEnv {
    fn drop(&mut self) {
        self.close();
        let _ = std::fs::remove_dir_all(&self.root);
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
    let mut s = String::from("@echo off\r\n");
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
    s += &format!("{}\r\nset RC=%ERRORLEVEL%\r\n", parts.join(" "));
    for o in &spec.outputs {
        plain("output path", &o.guest)?;
        plain("output name", &o.name)?;
        s += &format!(
            "if exist \"{g}\\*\" (xcopy \"{g}\" \"{GUEST_OUT}\\{n}\\\" /E /I /Q /Y >nul) else \
             (copy \"{g}\" \"{GUEST_OUT}\\{n}\" >nul)\r\n",
            g = o.guest,
            n = o.name
        );
    }
    s += &format!("echo %RC%> \"{GUEST_OUT}\\exit.code\"\r\nshutdown /s /t 0\r\n");
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

    #[test]
    fn missing_feature_is_reported_not_repaired() {
        if !Path::new(SANDBOX_EXE).exists() {
            let e = WindowsSandbox.provision(&spec("")).err().unwrap();
            assert!(matches!(e, LaunchError::NotProvisioned { .. }), "{e}");
        }
    }
}
