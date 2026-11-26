// SPDX-License-Identifier: MIT OR Apache-2.0
//! The VM-backed runner path (ROADMAP row 40, `docs/RUNNER-VM-PATH.md`): one runner in one Hyper-V VM that is a differencing
//! child of a read-only base, with no checkpoints, one NIC on a pool switch (no route off the host; the egress proxy is its only
//! way out, `docs/RUNNER-NETWORK.md`), destroyed when the runner ends.
//!
//! Two pieces behind seams, so every ordering rule is tested without Hyper-V:
//! * [`VmBackend`] is a [`RunnerBackend`]: verify the base once, create the child, start it, wait for the guest, call `started` with the
//!   VM's name (the transport then delivers the JIT config), wait for the runner to end, and **always** destroy the VM, whatever failed.
//! * [`VmHost`] is what acts on Hyper-V. [`ScriptHost`] builds each PowerShell script as text (quoted, prefix-checked, tested as text) and
//!   hands it to a [`Runner`]; nothing here has touched Hyper-V.
//!
//! **The guest channel** is PowerShell Direct with a **fresh password per VM** (PM ruling 2026-11-26; `adapter::guest_secret`): made
//! when the VM starts, handed to the host PowerShell on stdin, stored only as a DPAPI LocalMachine blob in the pool state folder and
//! deleted with the VM. Each guest script builds its `PSCredential` from that VM's blob inside the PowerShell process: the password
//! is never in the script text, an argument, a log or a result.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::dispatch::{JitTransport, RunnerBackend};
use super::github::{RunnerGroup, Secret};
use crate::broker_proto::{VM_PREFIX, valid_vm_id};
use crate::hyperv::{Runner, quote};

/// The Hyper-V data-exchange (KVP) item that carries a VM's password to its guest; the guest startup task reads it from
/// `HKLM:\SOFTWARE\Microsoft\Virtual Machine\External`. The name is not secret.
pub const KVP_NAME: &str = "antspawn-guest-pw";
const KVP_NS_ROOT: &str = "root\\virtualization\\v2";
const KVP_NS: &str = "\\\\.\\root\\virtualization\\v2";

/// What acts on Hyper-V for one runner VM. Every method may fail with a reason; `destroy` is idempotent (a VM already gone is `Ok`).
pub trait VmHost: Send + Sync + 'static {
    /// Check the base disk against its pinned hash. Called once per backend, before the first VM.
    fn verify_base(&self) -> Result<(), String>;
    fn create(&self, vm: &str) -> Result<(), String>;
    fn start(&self, vm: &str) -> Result<(), String>;
    /// Give this VM its own Administrator password: store it for the host side and put it on the VM bus for the guest. Called once per
    /// VM, after `start`, with a password made for that VM alone.
    fn provision(&self, vm: &str, secret: &Secret) -> Result<(), String>;
    /// Wait until the guest answers (heartbeat), at most `limit`.
    fn wait_ready(&self, vm: &str, limit: Duration) -> Result<(), String>;
    /// Wait until the runner has ended, at most `limit`; `Ok` is a one-line account.
    fn wait_end(&self, vm: &str, limit: Duration) -> Result<String, String>;
    fn destroy(&self, vm: &str) -> Result<(), String>;
}

/// One runner per VM.
pub struct VmBackend<H: VmHost> {
    pub host: H,
    /// Longest a runner may live (`AdapterConfig::clamp_time`).
    pub limit: Duration,
    /// Longest the guest may take to answer after start.
    pub ready: Duration,
    verified: OnceLock<()>,
    seq: AtomicU64,
    gate: Mutex<()>,
}

impl<H: VmHost> VmBackend<H> {
    pub fn new(host: H, limit: Duration, ready: Duration) -> Self {
        Self {
            host,
            limit,
            ready,
            verified: OnceLock::new(),
            seq: AtomicU64::new(0),
            gate: Mutex::new(()),
        }
    }

    /// `antspawn-run-<group>-<n>`: the group name reduced to lowercase letters, digits and hyphens, so the name is always a valid VM id.
    fn vm_name(&self, group: &RunnerGroup) -> Result<String, String> {
        let g: String = group
            .name
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .take(24)
            .collect();
        let n = self.seq.fetch_add(1, Ordering::SeqCst);
        let name = format!("{VM_PREFIX}run-{g}-{n}");
        if valid_vm_id(&name) {
            Ok(name)
        } else {
            Err(format!("cannot make a VM name for group `{}`", group.name))
        }
    }

    fn teardown(&self, vm: &str, result: Result<String, String>) -> Result<String, String> {
        match (result, self.host.destroy(vm)) {
            (r, Ok(())) => r,
            (Ok(_), Err(d)) => Err(format!("the runner ended but VM {vm} was not removed: {d}")),
            (Err(e), Err(d)) => Err(format!("{e}; and VM {vm} was not removed: {d}")),
        }
    }
}

impl<H: VmHost> RunnerBackend for VmBackend<H> {
    fn run_one(
        &self,
        group: &RunnerGroup,
        started: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<String, String> {
        // the base is hashed once; two runners never race the check
        {
            let _g = self.gate.lock().map_err(|_| "poisoned".to_string())?;
            if self.verified.get().is_none() {
                self.host.verify_base()?;
                let _ = self.verified.set(());
            }
        }
        let vm = self.vm_name(group)?;
        // from here a partly made VM may exist: every exit path destroys it
        let up = (|| {
            self.host.create(&vm)?;
            self.host.start(&vm)?;
            self.host.provision(&vm, &super::guest_secret::fresh()?)?;
            self.host.wait_ready(&vm, self.ready)?;
            started(&vm)?;
            self.host.wait_end(&vm, self.limit)
        })();
        self.teardown(&vm, up)
    }
}

/// Where the base and the VMs live, and what a VM looks like. Set by whoever starts the adapter, never by a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmHostConfig {
    /// The read-only base disk (`core-vendor-base.vhdx`), full path.
    pub base: String,
    /// Expected SHA-256 (64 hex digits) of the base. A base nobody pinned is not booted from.
    pub base_sha256: String,
    /// Folder the adapter's account has full control of: the differencing disks and VM files.
    pub vm_dir: String,
    /// The pool switch's name (Internal, forwarding off).
    pub switch: String,
    pub memory_mb: u32,
    pub vcpus: u8,
    /// Folder of the per-VM password blobs (`guest_secret::default_dir`): Administrators and SYSTEM only.
    pub secret_dir: std::path::PathBuf,
}

/// The real [`VmHost`]: scripts for a [`Runner`] (PowerShell on stdin). Nothing here has touched Hyper-V.
pub struct ScriptHost<R: Runner + Send + Sync + 'static> {
    pub runner: R,
    pub cfg: VmHostConfig,
}

fn join(dir: &str, leaf: &str) -> String {
    format!("{}\\{}", dir.trim_end_matches('\\'), leaf)
}

fn named(vm: &str) -> Result<String, String> {
    if valid_vm_id(vm) {
        quote(vm)
    } else {
        Err(format!(
            "refusing a VM name without the {VM_PREFIX} prefix or with odd characters"
        ))
    }
}

impl<R: Runner + Send + Sync + 'static> ScriptHost<R> {
    fn disk(&self, vm: &str) -> String {
        join(&self.cfg.vm_dir, &format!("{vm}.vhdx"))
    }

    /// Hash the base; a mismatch throws before any disk or VM exists.
    pub fn script_verify(&self) -> Result<String, String> {
        let h = &self.cfg.base_sha256;
        if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("the base hash is not 64 hex digits".into());
        }
        Ok(format!(
            "$ErrorActionPreference='Stop'\nif ((Get-FileHash -Algorithm SHA256 -LiteralPath {}).Hash -ne {}) {{ throw 'base image does not match its pinned SHA-256' }}\n",
            quote(&self.cfg.base)?,
            quote(&h.to_ascii_uppercase())?
        ))
    }

    /// A differencing child of the base, a generation 2 VM with one NIC on the pool switch, static memory, no checkpoints,
    /// only the heartbeat and key-value-exchange integration services (PowerShell Direct rides the VM bus, not an integration service;
    /// the key-value exchange is how the guest receives its own password, `guest_secret`). Not started.
    pub fn script_create(&self, vm: &str) -> Result<String, String> {
        let n = named(vm)?;
        let base = quote(&self.cfg.base)?;
        let disk = quote(&self.disk(vm))?;
        let switch = quote(&self.cfg.switch)?;
        let mem = u64::from(self.cfg.memory_mb) * 1024 * 1024;
        Ok(format!(
            "$ErrorActionPreference='Stop'\n\
             New-VHD -Path {disk} -ParentPath {base} -Differencing | Out-Null\n\
             New-VM -Name {n} -Generation 2 -MemoryStartupBytes {mem} -VHDPath {disk} -SwitchName {switch} -Path {vmdir} | Out-Null\n\
             Set-VMMemory -VMName {n} -DynamicMemoryEnabled $false\n\
             Set-VMProcessor -VMName {n} -Count {cpus} -ExposeVirtualizationExtensions $false\n\
             Get-VMIntegrationService -VMName {n} | Where-Object {{ $_.Name -notin 'Heartbeat', 'Key-Value Pair Exchange' }} | Disable-VMIntegrationService\n\
             Set-VM -Name {n} -CheckpointType Disabled\n",
            vmdir = quote(&self.cfg.vm_dir)?,
            cpus = self.cfg.vcpus,
        ))
    }

    pub fn script_start(&self, vm: &str) -> Result<String, String> {
        Ok(format!(
            "$ErrorActionPreference='Stop'\nStart-VM -Name {}\n",
            named(vm)?
        ))
    }

    /// Poll the heartbeat until OK, then PowerShell Direct (with the DPAPI credential) until the guest answers a command: the heartbeat
    /// comes up before the guest accepts a session (measured 2026-11-22), so only the second answer means `started` may deliver.
    /// Exit non-zero if either does not arrive within `secs` in all.
    pub fn script_ready(&self, vm: &str, secs: u64) -> Result<String, String> {
        let n = named(vm)?;
        let pre = crate::adapter::guest_secret::credential_preamble(&self.cfg.secret_dir, vm)?;
        let seal = self.script_seal(vm)?;
        Ok(format!(
            "$ErrorActionPreference='Stop'\n{pre}$end = (Get-Date).AddSeconds({secs})\n\
             while ((Get-VMIntegrationService -VMName {n} -Name Heartbeat).PrimaryStatusDescription -ne 'OK') {{\n\
             \x20 if ((Get-Date) -gt $end) {{ throw 'no heartbeat in time' }}\n  Start-Sleep -Seconds 1\n}}\n\
             while ($true) {{\n\
             \x20 try {{ if (Invoke-Command -VMName {n} -Credential $cred -ScriptBlock {{ 'up' }} -ErrorAction Stop) {{ break }} }} catch {{ }}\n\
             \x20 if ((Get-Date) -gt $end) {{ throw 'the guest did not answer PowerShell Direct in time' }}\n  Start-Sleep -Seconds 2\n}}\n{seal}"
        ))
    }

    /// Take the password off the VM bus once the guest has answered with it, so the job cannot read it from the guest registry. The
    /// item is removed by name; no value is needed.
    fn script_seal(&self, vm: &str) -> Result<String, String> {
        let n = named(vm)?;
        Ok(format!(
            "$vmo = Get-WmiObject -Namespace {KVP_NS_ROOT} -Class Msvm_ComputerSystem -Filter \"ElementName={n}\"\n\
             $svc = Get-WmiObject -Namespace {KVP_NS_ROOT} -Class Msvm_VirtualSystemManagementService\n\
             $item = ([wmiclass]'{KVP_NS}:Msvm_KvpExchangeDataItem').CreateInstance()\n\
             $item.Name = '{KVP_NAME}'; $item.Data = ''; $item.Source = 0\n\
             $r = $svc.RemoveKvpItems($vmo, @($item.PSBase.GetText(1)))\n\
             if ($r.ReturnValue -ne 0 -and $r.ReturnValue -ne 4096) {{ throw \"could not take the guest password off the VM bus ($($r.ReturnValue))\" }}\n"
        ))
    }

    /// Reads the password from **stdin**, stores it as a LocalMachine DPAPI blob in this VM's file, and puts it on the VM bus as a data
    /// exchange item that the guest's startup task (`scripts/guest-bootstrap.ps1`) reads. The script carries names and paths only.
    pub fn script_provision(&self, vm: &str) -> Result<String, String> {
        let n = named(vm)?;
        let file = quote(
            &crate::adapter::guest_secret::vm_file(&self.cfg.secret_dir, vm)?
                .display()
                .to_string(),
        )?;
        let dir = quote(&self.cfg.secret_dir.display().to_string())?;
        Ok(format!(
            "$ErrorActionPreference='Stop'\n\
             $pw = [Console]::In.ReadToEnd().Trim()\n\
             if (-not $pw) {{ throw 'no guest password on stdin' }}\n\
             try {{\n\
             \x20 Add-Type -AssemblyName System.Security\n\
             \x20 if (-not (Test-Path -LiteralPath {dir})) {{ New-Item -ItemType Directory -Path {dir} | Out-Null }}\n\
             \x20 $blob = [Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes($pw), $null, 'LocalMachine')\n\
             \x20 [IO.File]::WriteAllText({file}, (($blob | ForEach-Object {{ $_.ToString('x2') }}) -join ''))\n\
             \x20 $vmo = Get-WmiObject -Namespace {KVP_NS_ROOT} -Class Msvm_ComputerSystem -Filter \"ElementName={n}\"\n\
             \x20 $svc = Get-WmiObject -Namespace {KVP_NS_ROOT} -Class Msvm_VirtualSystemManagementService\n\
             \x20 $item = ([wmiclass]'{KVP_NS}:Msvm_KvpExchangeDataItem').CreateInstance()\n\
             \x20 $item.Name = '{KVP_NAME}'; $item.Data = $pw; $item.Source = 0\n\
             \x20 $r = $svc.AddKvpItems($vmo, @($item.PSBase.GetText(1)))\n\
             \x20 if ($r.ReturnValue -ne 0 -and $r.ReturnValue -ne 4096) {{ throw \"could not put the guest password on the VM bus ($($r.ReturnValue))\" }}\n\
             }} finally {{ $pw = $null }}\n"
        ))
    }

    /// Over PowerShell Direct: wait for `Runner.Listener` to appear in the guest, then for it to be gone (or the VM to be off), at
    /// most `secs` from now. The credential comes from the DPAPI file inside this script; nothing here carries the password.
    pub fn script_wait_end(&self, vm: &str, secs: u64) -> Result<String, String> {
        let n = named(vm)?;
        let pre = crate::adapter::guest_secret::credential_preamble(&self.cfg.secret_dir, vm)?;
        Ok(format!(
            "$ErrorActionPreference='Stop'
{pre}$end = (Get-Date).AddSeconds({secs})
$seen = $false
             while ($true) {{
               if ((Get-VM -Name {n}).State -ne 'Running') {{ break }}
               $up = $false
               try {{ $up = [bool](Invoke-Command -VMName {n} -Credential $cred -ScriptBlock {{ Get-Process -Name 'Runner.Listener' -ErrorAction SilentlyContinue }}) }} catch {{ if ($seen) {{ break }} }}
               if ($up) {{ $seen = $true }} elseif ($seen) {{ break }}
               if ((Get-Date) -gt $end) {{ throw 'the runner did not end in time' }}
               Start-Sleep -Seconds 2
}}
"
        ))
    }

    /// Idempotent, and removes only what carries our prefix: the VM (power cut), its differencing disk.
    pub fn script_destroy(&self, vm: &str) -> Result<String, String> {
        let n = named(vm)?;
        let disk = quote(&self.disk(vm))?;
        // `New-VM -Path` leaves an empty folder named for the VM; remove it only when it holds no file
        let folder = quote(&join(&self.cfg.vm_dir, vm))?;
        // the VM's own password blob goes with it (only that VM's file)
        let blob = quote(
            &crate::adapter::guest_secret::vm_file(&self.cfg.secret_dir, vm)?
                .display()
                .to_string(),
        )?;
        Ok(format!(
            "$ErrorActionPreference='Stop'\n\
             $v = Get-VM -Name {n} -ErrorAction SilentlyContinue\n\
             if ($v) {{ if ($v.State -ne 'Off') {{ Stop-VM -Name {n} -TurnOff -Force }}; Remove-VM -Name {n} -Force }}\n\
             if (Test-Path -LiteralPath {disk}) {{ Remove-Item -LiteralPath {disk} -Force }}\n\
             if ((Test-Path -LiteralPath {folder}) -and -not (Get-ChildItem -LiteralPath {folder} -Force -Recurse -File)) {{ Remove-Item -LiteralPath {folder} -Recurse -Force }}\n\
             if (Test-Path -LiteralPath {blob}) {{ Remove-Item -LiteralPath {blob} -Force }}\n"
        ))
    }
}

impl<R: Runner + Send + Sync + 'static> VmHost for ScriptHost<R> {
    fn verify_base(&self) -> Result<(), String> {
        self.runner.run(&self.script_verify()?).map(|_| ())
    }
    fn create(&self, vm: &str) -> Result<(), String> {
        self.runner.run(&self.script_create(vm)?).map(|_| ())
    }
    fn start(&self, vm: &str) -> Result<(), String> {
        self.runner.run(&self.script_start(vm)?).map(|_| ())
    }
    fn provision(&self, vm: &str, secret: &Secret) -> Result<(), String> {
        let script = self.script_provision(vm)?;
        // an error text never carries the password, whatever the runner put in it
        self.runner
            .run_with_input(&script, secret.expose())
            .map(|_| ())
            .map_err(|e| e.replace(secret.expose(), "<redacted>"))
    }
    fn wait_ready(&self, vm: &str, limit: Duration) -> Result<(), String> {
        self.runner
            .run(&self.script_ready(vm, limit.as_secs().max(1))?)
            .map(|_| ())
    }
    fn wait_end(&self, vm: &str, limit: Duration) -> Result<String, String> {
        self.runner
            .run(&self.script_wait_end(vm, limit.as_secs().max(1))?)
            .map(|_| "runner ended".to_string())
    }
    fn destroy(&self, vm: &str) -> Result<(), String> {
        self.runner.run(&self.script_destroy(vm)?).map(|_| ())
    }
}

/// The one-time runner registration into the guest, over PowerShell Direct (ROADMAP row 40; PM task 2026-11-22). The JIT config
/// reaches the host PowerShell on its **stdin** and the guest as an `Invoke-Command` argument over the VM bus; there it becomes
/// `ACTIONS_RUNNER_INPUT_JITCONFIG` in the one child that starts the runner, and the guest variable holding it is cleared at once.
/// It is never in a script, a command line, a file, a log, a status file or the disk image, and an error never carries it.
pub struct VmTransport<R: Runner + Send + Sync + 'static> {
    pub runner: R,
    /// Folder of the per-VM password blobs (`guest_secret::default_dir`).
    pub secret_dir: std::path::PathBuf,
    /// The runner folder in the guest (the image's `C:\runner`, holding `run.cmd`).
    pub runner_dir: String,
}

/// A JIT config is about 4 KiB; the same limits as `spawn-jit-shim`: non-empty, at most 64 KiB, base64 characters only.
fn plausible_jit(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64 * 1024
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'))
}

impl<R: Runner + Send + Sync + 'static> VmTransport<R> {
    /// The script that takes the config from stdin. It holds no secret: only names and paths.
    pub fn script_deliver(&self, vm: &str) -> Result<String, String> {
        let n = named(vm)?;
        let pre = crate::adapter::guest_secret::credential_preamble(&self.secret_dir, vm)?;
        let dir = quote(&self.runner_dir)?;
        Ok(format!(
            "$ErrorActionPreference='Stop'\n{pre}$j = [Console]::In.ReadToEnd().Trim()\n\
             if (-not $j) {{ throw 'no JIT config on stdin' }}\n\
             try {{\n\
             \x20 Invoke-Command -VMName {n} -Credential $cred -ArgumentList $j,{dir} -ScriptBlock {{\n\
             \x20   param($j, $dir)\n\
             \x20   $psi = New-Object System.Diagnostics.ProcessStartInfo 'cmd.exe'\n\
             \x20   $psi.Arguments = '/c run.cmd'\n\
             \x20   $psi.WorkingDirectory = $dir\n\
             \x20   $psi.UseShellExecute = $false\n\
             \x20   $psi.EnvironmentVariables['ACTIONS_RUNNER_INPUT_JITCONFIG'] = $j\n\
             \x20   $j = $null\n\
             \x20   $p = [System.Diagnostics.Process]::Start($psi)\n\
             \x20   $psi = $null\n\
             \x20   'started'\n\
             \x20 }}\n\
             }} finally {{ $j = $null }}\n"
        ))
    }
}

impl<R: Runner + Send + Sync + 'static> JitTransport for VmTransport<R> {
    fn deliver(&self, vm: &str, jit: &Secret) -> Result<(), String> {
        if !plausible_jit(jit.expose()) {
            return Err(
                "the JIT config is empty, oversized or not base64; refused before it left the host"
                    .into(),
            );
        }
        let script = self.script_deliver(vm)?;
        match self.runner.run_with_input(&script, jit.expose()) {
            Ok(out) if out.trim() == "started" => Ok(()),
            Ok(_) => Err("the guest did not confirm the runner started".into()),
            // whatever the failure text says, it never carries the config
            Err(e) => Err(e.replace(jit.expose(), "<redacted>")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Default)]
    struct Fake {
        calls: Arc<Mutex<Vec<String>>>,
        secrets: Arc<Mutex<Vec<String>>>,
        fail_at: Option<&'static str>,
        destroy_fails: bool,
    }
    impl Fake {
        fn step(&self, what: &str, vm: &str) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{what} {vm}").trim().to_string());
            if self.fail_at == Some(what) {
                Err(format!("{what} failed"))
            } else {
                Ok(())
            }
        }
    }
    impl VmHost for Fake {
        fn verify_base(&self) -> Result<(), String> {
            self.step("verify", "")
        }
        fn create(&self, vm: &str) -> Result<(), String> {
            self.step("create", vm)
        }
        fn start(&self, vm: &str) -> Result<(), String> {
            self.step("start", vm)
        }
        fn provision(&self, vm: &str, secret: &Secret) -> Result<(), String> {
            self.secrets
                .lock()
                .unwrap()
                .push(secret.expose().to_string());
            self.step("provision", vm)
        }
        fn wait_ready(&self, vm: &str, _: Duration) -> Result<(), String> {
            self.step("ready", vm)
        }
        fn wait_end(&self, vm: &str, _: Duration) -> Result<String, String> {
            self.step("end", vm).map(|()| "runner ended".into())
        }
        fn destroy(&self, vm: &str) -> Result<(), String> {
            self.step("destroy", vm)?;
            if self.destroy_fails {
                Err("locked".into())
            } else {
                Ok(())
            }
        }
    }

    fn group() -> RunnerGroup {
        RunnerGroup {
            name: "Antspawn Account".into(),
            repos: vec!["O/a".into()],
            labels: vec!["x".into()],
            launcher: "hyperv-vm".into(),
            task: None,
            pipe: None,
            egress: Vec::new(),
            egress_listen: Vec::new(),
        }
    }

    fn backend(f: Fake) -> (VmBackend<Fake>, Arc<Mutex<Vec<String>>>) {
        let calls = f.calls.clone();
        (
            VmBackend::new(f, Duration::from_secs(60), Duration::from_secs(30)),
            calls,
        )
    }

    #[test]
    fn the_whole_life_in_order_and_the_vm_is_always_destroyed() {
        let (b, calls) = backend(Fake::default());
        let mut told = Vec::new();
        let r = b.run_one(&group(), &mut |vm| {
            told.push(vm.to_string());
            Ok(())
        });
        assert_eq!(r.unwrap(), "runner ended");
        let c = calls.lock().unwrap().clone();
        let vm = "antspawn-run-antspawn-account-0";
        assert_eq!(
            c,
            [
                "verify",
                &format!("create {vm}"),
                &format!("start {vm}"),
                &format!("provision {vm}"),
                &format!("ready {vm}"),
                &format!("end {vm}"),
                &format!("destroy {vm}")
            ]
        );
        assert_eq!(told, [vm]);
        assert!(valid_vm_id(vm));
    }

    #[test]
    fn the_base_is_verified_once_and_each_runner_gets_its_own_vm() {
        let (b, calls) = backend(Fake::default());
        b.run_one(&group(), &mut |_| Ok(())).unwrap();
        b.run_one(&group(), &mut |_| Ok(())).unwrap();
        let c = calls.lock().unwrap().clone();
        assert_eq!(c.iter().filter(|s| *s == "verify").count(), 1);
        assert!(
            c.iter()
                .any(|s| s == "create antspawn-run-antspawn-account-1")
        );
    }

    #[test]
    fn a_failure_at_any_step_still_destroys_the_vm_and_says_which_step() {
        for step in ["create", "start", "ready", "end"] {
            let (b, calls) = backend(Fake {
                fail_at: Some(step),
                ..Fake::default()
            });
            let e = b.run_one(&group(), &mut |_| Ok(())).unwrap_err();
            assert!(e.contains(&format!("{step} failed")), "{step}: {e}");
            let c = calls.lock().unwrap().clone();
            assert_eq!(
                c.last().unwrap(),
                "destroy antspawn-run-antspawn-account-0",
                "{step}: {c:?}"
            );
        }
    }

    #[test]
    fn a_delivery_that_fails_ends_the_vm_and_returns_that_error() {
        let (b, calls) = backend(Fake::default());
        let e = b
            .run_one(&group(), &mut |_| Err("could not deliver".into()))
            .unwrap_err();
        assert_eq!(e, "could not deliver");
        let c = calls.lock().unwrap().clone();
        assert!(
            !c.iter().any(|s| s.starts_with("end")),
            "must not wait for a runner that never got its config: {c:?}"
        );
        assert!(c.last().unwrap().starts_with("destroy"));
    }

    #[test]
    fn a_base_that_fails_its_hash_creates_nothing_and_is_checked_again_next_time() {
        let (b, calls) = backend(Fake {
            fail_at: Some("verify"),
            ..Fake::default()
        });
        assert!(
            b.run_one(&group(), &mut |_| Ok(()))
                .unwrap_err()
                .contains("verify failed")
        );
        assert!(b.run_one(&group(), &mut |_| Ok(())).is_err());
        let c = calls.lock().unwrap().clone();
        assert_eq!(c, ["verify", "verify"], "no VM, no destroy, no cached pass");
    }

    #[test]
    fn a_vm_that_cannot_be_removed_is_reported_even_when_the_runner_ended_well() {
        let (b, _) = backend(Fake {
            destroy_fails: true,
            ..Fake::default()
        });
        let e = b.run_one(&group(), &mut |_| Ok(())).unwrap_err();
        assert!(e.contains("was not removed") && e.contains("locked"), "{e}");
        let (b, _) = backend(Fake {
            fail_at: Some("end"),
            destroy_fails: true,
            ..Fake::default()
        });
        let e = b.run_one(&group(), &mut |_| Ok(())).unwrap_err();
        assert!(
            e.contains("end failed") && e.contains("was not removed"),
            "{e}"
        );
    }

    // ---- the scripts, as text ----

    struct Rec(Mutex<Vec<String>>);
    impl Runner for Rec {
        fn run(&self, script: &str) -> Result<String, String> {
            self.0.lock().unwrap().push(script.to_string());
            Ok(String::new())
        }
        fn run_with_input(&self, script: &str, input: &str) -> Result<String, String> {
            self.0
                .lock()
                .unwrap()
                .push(format!("{script}\n<<stdin:{input}>>"));
            Ok(String::new())
        }
    }

    fn host() -> ScriptHost<Rec> {
        ScriptHost {
            runner: Rec(Mutex::new(Vec::new())),
            cfg: VmHostConfig {
                base: r"C:\vmimages\pilot\core-vendor-base.vhdx".into(),
                base_sha256: "ab".repeat(32),
                vm_dir: r"C:\ProgramData\antspawn-vms".into(),
                switch: "antspawn-pool-1".into(),
                memory_mb: 4096,
                vcpus: 2,
                secret_dir: std::path::PathBuf::from(r"C:\ProgramData\antspawn-pool\guest"),
            },
        }
    }

    #[test]
    fn create_makes_a_differencing_child_on_the_pool_switch_with_no_checkpoints() {
        let s = host().script_create("antspawn-run-g-0").unwrap();
        assert!(
            s.contains("-ParentPath 'C:\\vmimages\\pilot\\core-vendor-base.vhdx' -Differencing"),
            "{s}"
        );
        assert!(
            s.contains("-SwitchName 'antspawn-pool-1'") && s.contains("-Generation 2"),
            "{s}"
        );
        assert!(
            s.contains("-CheckpointType Disabled") && s.contains("-MemoryStartupBytes 4294967296"),
            "{s}"
        );
        assert!(
            s.contains("-notin 'Heartbeat', 'Key-Value Pair Exchange'"),
            "every integration service but the heartbeat is off: {s}"
        );
        assert!(
            !s.contains("Add-VMNetworkAdapter"),
            "exactly one NIC: the one New-VM puts on the switch"
        );
    }

    #[test]
    fn names_without_the_prefix_or_with_odd_characters_are_refused_by_every_script() {
        let h = host();
        for bad in [
            "win",
            "antspawn-",
            "antspawn-x'; Remove-VM *",
            "antspawn-a b",
            "antspawn-a\nb",
        ] {
            assert!(h.script_create(bad).is_err(), "{bad:?}");
            assert!(h.script_start(bad).is_err(), "{bad:?}");
            assert!(h.script_ready(bad, 5).is_err(), "{bad:?}");
            assert!(h.script_destroy(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn destroy_is_idempotent_and_touches_only_that_vm_and_its_disk() {
        let s = host().script_destroy("antspawn-run-g-0").unwrap();
        assert!(
            s.contains("-ErrorAction SilentlyContinue") && s.contains("Test-Path"),
            "{s}"
        );
        assert!(s.contains("antspawn-run-g-0.vhdx"), "{s}");
        assert!(
            !s.contains("Get-VM |") && !s.contains("*"),
            "never a bulk removal: {s}"
        );
    }

    #[test]
    fn verify_refuses_a_base_with_no_valid_pin_and_pins_the_uppercase_hash() {
        let s = host().script_verify().unwrap();
        assert!(s.contains(&"AB".repeat(32)) && s.contains("throw"), "{s}");
        let mut h = host();
        h.cfg.base_sha256 = "short".into();
        assert!(h.script_verify().is_err());
    }

    #[test]
    fn a_values_with_a_quote_or_newline_cannot_end_a_literal() {
        let mut h = host();
        h.cfg.switch = "pool'; Stop-Computer; '".into();
        let s = h.script_create("antspawn-run-g-0").unwrap();
        assert!(
            s.contains("'pool''; Stop-Computer; '''"),
            "quote doubled inside one literal: {s}"
        );
        h.cfg.switch = "pool\nx".into();
        assert!(h.script_create("antspawn-run-g-0").is_err());
    }

    #[test]
    fn the_wait_script_uses_the_dpapi_file_and_never_a_password() {
        let h = host();
        let s = h.script_wait_end("antspawn-run-g-0", 600).unwrap();
        assert!(
            s.contains("antspawn-run-g-0.dpapi")
                && s.contains("'LocalMachine'")
                && s.contains("Invoke-Command -VMName 'antspawn-run-g-0'"),
            "{s}"
        );
        assert!(
            s.contains("AddSeconds(600)") && s.contains("Runner.Listener"),
            "{s}"
        );
        assert!(!s.contains("-AsPlainText") && !s.contains("PW"), "{s}");
        assert!(h.script_wait_end("other-vm", 5).is_err());
        h.wait_end("antspawn-run-g-0", Duration::from_secs(5))
            .unwrap();
        assert_eq!(h.runner.0.lock().unwrap().len(), 1);
    }

    // ---- the per-guest password (PM ruling 2026-11-26) ----

    const PW: &str = "Aa1-0123456789abcdef-test-only-not-a-real-password";

    #[test]
    fn two_runners_get_two_different_passwords_and_each_is_made_after_start() {
        let f = Fake::default();
        let secrets = f.secrets.clone();
        let (b, _) = backend(f);
        b.run_one(&group(), &mut |_| Ok(())).unwrap();
        b.run_one(&group(), &mut |_| Ok(())).unwrap();
        let s = secrets.lock().unwrap().clone();
        assert_eq!(s.len(), 2, "one password per VM");
        assert_ne!(s[0], s[1], "a password was reused across builds");
        assert!(s.iter().all(|p| p.len() >= 4 + 64), "{s:?}");
    }

    #[test]
    fn the_password_is_on_stdin_never_in_the_script_and_never_in_an_error() {
        let h = host();
        let script = h.script_provision("antspawn-run-g-0").unwrap();
        assert!(
            !script.contains(PW) && !script.contains("-AsPlainText"),
            "{script}"
        );
        assert!(
            script.contains("[Console]::In.ReadToEnd()")
                && script.contains("'LocalMachine'")
                && script.contains("antspawn-run-g-0.dpapi")
                && script.contains("$pw = $null"),
            "{script}"
        );
        // nothing in it prints, logs or exports the password
        for bad in [
            "Write-Host",
            "Write-Output",
            "Out-File",
            "Set-Content",
            "$env:",
            "Start-Process",
            "-ArgumentList",
        ] {
            assert!(!script.contains(bad), "{bad} in {script}");
        }
        h.provision("antspawn-run-g-0", &Secret::new(PW.into()))
            .unwrap();
        let seen = h.runner.0.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        let (text, stdin) = seen[0].split_once("\n<<stdin:").unwrap();
        assert_eq!(stdin, format!("{PW}>>"), "the password is the stdin");
        assert!(!text.contains(PW), "password in the script text");
        // control: the same check sees the password when it is there
        assert!(seen[0].contains(PW));
        assert!(h.script_provision("not-ours").is_err());
    }

    struct Leaky;
    impl Runner for Leaky {
        fn run(&self, _: &str) -> Result<String, String> {
            Err("no".into())
        }
        fn run_with_input(&self, _: &str, input: &str) -> Result<String, String> {
            Err(format!("powershell exited 1: echoed {input} back"))
        }
    }

    #[test]
    fn a_provisioning_failure_never_carries_the_password() {
        let h = ScriptHost {
            runner: Leaky,
            cfg: host().cfg,
        };
        let e = h
            .provision("antspawn-run-g-0", &Secret::new(PW.into()))
            .unwrap_err();
        assert!(!e.contains(PW) && e.contains("<redacted>"), "{e}");
        // control: the raw runner error did carry it
        assert!(Leaky.run_with_input("", PW).unwrap_err().contains(PW));
    }

    #[test]
    fn the_password_comes_off_the_bus_once_the_guest_answered_and_its_blob_goes_with_the_vm() {
        let h = host();
        let r = h.script_ready("antspawn-run-g-0", 60).unwrap();
        let answered = r.rfind("did not answer PowerShell Direct").unwrap();
        let sealed = r.find("RemoveKvpItems").unwrap();
        assert!(
            sealed > answered,
            "the item is removed only after the guest answered: {r}"
        );
        assert!(!r.contains(PW));
        let d = h.script_destroy("antspawn-run-g-0").unwrap();
        assert!(
            d.contains("antspawn-run-g-0.dpapi") && !d.contains("antspawn-run-g-1"),
            "{d}"
        );
        // the blob is removed AFTER the VM, so an unwritable folder cannot keep a VM alive
        assert!(
            d.find("Remove-VM").unwrap() < d.find("antspawn-run-g-0.dpapi").unwrap(),
            "{d}"
        );
    }

    // ---- the JIT transport ----

    const JIT: &str = "eyJkb19ub3RfbG9nIjoiVEhJUy1JUy1BLUpJVC1TRUNSRVQifQ==";

    /// Records the script and the stdin it was given; fails when told to, with the secret in its failure text (the worst case).
    struct Spy {
        seen: Mutex<Vec<(String, String)>>,
        reply: Result<String, String>,
    }
    impl Runner for Spy {
        fn run(&self, _: &str) -> Result<String, String> {
            Err("no".into())
        }
        fn run_with_input(&self, script: &str, input: &str) -> Result<String, String> {
            self.seen
                .lock()
                .unwrap()
                .push((script.to_string(), input.to_string()));
            self.reply.clone()
        }
    }
    fn transport(reply: Result<String, String>) -> VmTransport<Spy> {
        VmTransport {
            runner: Spy {
                seen: Mutex::new(Vec::new()),
                reply,
            },
            secret_dir: std::path::PathBuf::from(r"C:\ProgramData\antspawn-pool\guest"),
            runner_dir: r"C:\runner".into(),
        }
    }

    #[test]
    fn the_config_goes_on_stdin_and_in_no_script() {
        let t = transport(Ok("started\r\n".into()));
        t.deliver("antspawn-run-g-0", &Secret::new(JIT.into()))
            .unwrap();
        let seen = t.runner.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let (script, input) = &seen[0];
        assert_eq!(input, JIT, "the config is the stdin");
        assert!(!script.contains(JIT), "config in the script text");
        assert!(!script.contains("-AsPlainText"), "{script}");
        // control: the same check does see the config when it is present
        assert!(format!("{script}{input}").contains(JIT));
        assert!(
            script.contains("[Console]::In.ReadToEnd()")
                && script.contains("EnvironmentVariables['ACTIONS_RUNNER_INPUT_JITCONFIG']")
                && script.contains("$j = $null"),
            "{script}"
        );
    }

    #[test]
    fn a_failure_never_carries_the_config() {
        let t = transport(Err(format!("powershell exited 1: bad value {JIT} here")));
        let e = t
            .deliver("antspawn-run-g-0", &Secret::new(JIT.into()))
            .unwrap_err();
        assert!(!e.contains(JIT) && e.contains("<redacted>"), "{e}");
        // control: the unredacted failure did carry it
        assert!(t.runner.reply.clone().unwrap_err().contains(JIT));
    }

    #[test]
    fn an_unconfirmed_start_and_a_bad_config_are_refused() {
        let t = transport(Ok(String::new()));
        assert!(
            t.deliver("antspawn-run-g-0", &Secret::new(JIT.into()))
                .unwrap_err()
                .contains("did not confirm")
        );
        for bad in ["", "has space", "line\nbreak", "q'uote"] {
            let t = transport(Ok("started".into()));
            let e = t
                .deliver("antspawn-run-g-0", &Secret::new(bad.into()))
                .unwrap_err();
            assert!(e.contains("refused"), "{bad:?}: {e}");
            assert!(
                t.runner.seen.lock().unwrap().is_empty(),
                "{bad:?} left the host"
            );
        }
        assert!(
            transport(Ok("started".into()))
                .deliver("not-ours", &Secret::new(JIT.into()))
                .is_err()
        );
    }
}
