// SPDX-License-Identifier: MIT OR Apache-2.0
//! One real runner VM through `ScriptHost`, with the guest channel (ROADMAP row 40). Run as the interactive user who is in
//! Hyper-V Administrators; never elevates. Prints `PILOT` lines and always destroys the VM.
//!
//! `cargo run --example vm_pilot -- <base.vhdx> <base-sha256> <vm_dir> <switch>`
//!
//! Steps: verify the base hash, create a differencing child, start, wait for the heartbeat, then over PowerShell Direct with the
//! DPAPI credential run `hostname` in the guest, then `wait_end` with a short limit (no runner is started, so it must time
//! out), then destroy. The password is read only by the PowerShell it runs in; this program never sees it.

use std::time::{Duration, Instant};

use antspawn::adapter::dispatch::JitTransport;
use antspawn::adapter::github::Secret;
use antspawn::adapter::guest_secret;
use antspawn::adapter::vm_backend::{ScriptHost, VmHost, VmHostConfig, VmTransport};

/// A made-up value shaped like a JIT config. Never a real one.
const FAKE: &str = "RkFLRS1KSVQtQ09ORklHLU5PVC1SRUFMLTQyNDI0Mg==";
use antspawn::hyperv::{PowerShell, Runner};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() != 4 {
        eprintln!("usage: vm_pilot <base.vhdx> <base-sha256> <vm_dir> <switch>");
        std::process::exit(64);
    }
    let cred = guest_secret::default_dir().expect("ProgramData");
    println!("PILOT {}", guest_secret::check(&cred).unwrap_or_else(|e| e));
    let host = ScriptHost {
        runner: PowerShell,
        cfg: VmHostConfig {
            base: a[0].clone(),
            base_sha256: a[1].clone(),
            vm_dir: a[2].clone(),
            switch: a[3].clone(),
            memory_mb: 4096,
            vcpus: 2,
            secret_dir: cred.clone(),
        },
    };
    let vm = "antspawn-run-pilot-0";
    let t = Instant::now();
    let step = |name: &str, r: Result<String, String>| {
        let ok = r.is_ok();
        println!(
            "PILOT {name}: {} at {:.1}s {}",
            if ok { "ok" } else { "FAILED" },
            t.elapsed().as_secs_f32(),
            r.unwrap_or_else(|e| e)
        );
        ok
    };
    let ok = step("verify", host.verify_base().map(|()| String::new()))
        && step("create", host.create(vm).map(|()| String::new()))
        && step("start", host.start(vm).map(|()| String::new()))
        && step(
            "provision (fresh per-VM password)",
            guest_secret::fresh().and_then(|p| {
                host.provision(vm, &p).map(|()| String::new()).map_err(|_| {
                    "provision failed (text withheld: it handled the password)".to_string()
                })
            }),
        )
        && step(
            "ready",
            host.wait_ready(vm, Duration::from_secs(300))
                .map(|()| String::new()),
        )
        && step("guest-hostname", {
            let pre = guest_secret::credential_preamble(&cred, vm).unwrap();
            PowerShell.run(&format!(
                "$ErrorActionPreference='Stop'\n{pre}Invoke-Command -VMName '{vm}' -Credential $cred -ScriptBlock {{ hostname }}\n"
            ))
        })
        && step("deliver a FAKE config, runner starts", {
            let t = VmTransport {
                runner: PowerShell,
                secret_dir: cred.clone(),
                runner_dir: r"C:\runner".into(),
            };
            t.deliver(vm, &Secret::new(FAKE.into()))
                .map(|()| "started".into())
        })
        && step(
            "wait_end with the real runner (fake config: it fails and exits)",
            { host.wait_end(vm, Duration::from_secs(240)) },
        )
        && step("guest holds no trace of the fake config", {
            let pre = guest_secret::credential_preamble(&cred, vm).unwrap();
            PowerShell.run(&format!(
                "$ErrorActionPreference='Stop'\n{pre}Invoke-Command -VMName '{vm}' -Credential $cred -ArgumentList '{FAKE}' -ScriptBlock {{ param($f) $ErrorActionPreference='SilentlyContinue'; $hits = Get-ChildItem C:/runner, C:/Users, C:/ProgramData, C:/Windows/Temp -Recurse -File -Force -ErrorAction SilentlyContinue | Where-Object {{ $_.Length -lt 5MB }} | Select-String -SimpleMatch -Pattern $f -List -ErrorAction SilentlyContinue; 'files holding it: ' + @($hits).Count; $c = 'C:/Windows/Temp/antspawn-control.txt'; Set-Content -LiteralPath $c -Value $f; $ctl = Get-ChildItem C:/Windows/Temp -File -Force | Select-String -SimpleMatch -Pattern $f -List; Remove-Item -LiteralPath $c -Force; 'positive control (planted file) found: ' + @($ctl).Count; 'diag logs: ' + @(Get-ChildItem C:/runner/_diag -ErrorAction SilentlyContinue).Count }}\n"
            ))
        });
    let d = step("destroy", host.destroy(vm).map(|()| String::new()));
    std::process::exit(i32::from(!(ok && d)));
}
