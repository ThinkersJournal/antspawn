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

use antspawn::adapter::guest_credential;
use antspawn::adapter::vm_backend::{ScriptHost, VmHost, VmHostConfig};
use antspawn::hyperv::{PowerShell, Runner};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() != 4 {
        eprintln!("usage: vm_pilot <base.vhdx> <base-sha256> <vm_dir> <switch>");
        std::process::exit(64);
    }
    let cred = guest_credential::default_path().expect("LOCALAPPDATA");
    println!(
        "PILOT {}",
        guest_credential::check(&cred).unwrap_or_else(|e| e)
    );
    let host = ScriptHost {
        runner: PowerShell,
        cfg: VmHostConfig {
            base: a[0].clone(),
            base_sha256: a[1].clone(),
            vm_dir: a[2].clone(),
            switch: a[3].clone(),
            memory_mb: 4096,
            vcpus: 2,
            guest_credential: cred.clone(),
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
            "ready",
            host.wait_ready(vm, Duration::from_secs(300))
                .map(|()| String::new()),
        )
        && step("guest-hostname", {
            let pre = guest_credential::credential_preamble(&cred).unwrap();
            PowerShell.run(&format!(
                "$ErrorActionPreference='Stop'\n{pre}Invoke-Command -VMName '{vm}' -Credential $cred -ScriptBlock {{ hostname }}\n"
            ))
        })
        && step("wait_end(no runner, 20 s)", {
            match host.wait_end(vm, Duration::from_secs(20)) {
                Err(e) if e.contains("did not end in time") => Ok("timed out as expected".into()),
                other => Err(format!("unexpected: {other:?}")),
            }
        });
    let d = step("destroy", host.destroy(vm).map(|()| String::new()));
    std::process::exit(i32::from(!(ok && d)));
}
