<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# The VM path for network-needing runners (design note; V6/V7 of `docs/SANDBOX-BROKER.md`)

Written 2026-11-21 for the PM's `[TASK] vm-path-for-network-runners`. **Design only. Nothing here was built; every figure not marked "measured" is unmeasured.** Network policy and the proxy:
`docs/RUNNER-NETWORK.md`. Ruled (CireSnave 2026-11-21, verbatim): *"In that case, runners that need network access run in VM, not in Sandbox. That way we can cleanly limit their network access."*
Also ruled: the broker runs as `cires`.

## Shape

Adapter mints a JIT config -> the broker (as `cires`, a Hyper-V Administrator, not elevated) creates one VM from a fixed base -> the runner inside registers over the proxy-only network,
takes one job, exits -> the broker destroys the VM. One VM per job, ephemeral by construction (the JIT runner deregisters itself after one job).

## Network

* **A pool of Internal switches**, each a /30 (host `.1`, guest `.2`), one guest per switch, IPv4 forwarding Disabled, no NAT. Measured 2026-11-07/08: an Internal switch can be created by a
  non-elevated Hyper-V Administrator (`New-VMSwitch`, 7.3 s), forwarding is Disabled by default and `Get-NetNat` lists nothing; setting the host address on the vEthernet is elevated (setup, CireSnave's).
  `antspawn-v0-sw` (10.77.0.1/30) already exists from V0.
* **The proxy is the only reachable address:** the host's `.1`, port per group (`RUNNER-NETWORK.md`). Host Windows Firewall inbound rules on that interface block everything else (elevated setup).
* **DNS only via the proxy:** the guest has no DNS server configured; it names the proxy by IP literal and passes host names in `CONNECT`.
* A guest on one switch cannot reach another guest: no shared L2, no route. (Whether the host forwards between its vEthernets: forwarding Disabled, measured on the Default Switch and WSL switch.)

## Guest creation and teardown, per job

1. Broker receives `{group, base}` over its pipe (validated: names, not paths; V1).
2. `New-VHD -Differencing` child of the **read-only base VHDX** under the broker's job folder (about 1.1 s, measured on a 64 MB dynamic disk and its child; a real base is unmeasured).
3. A **seed**: a small ISO (or second VHDX) with per-VM config, written by the broker: unique computer name, the /30 address, the proxy address and port, a random local password for this VM only
   (never logged, never stored after the VM ends), and the runner start line.
4. `New-VM -Generation 2` with the child, the seed, one NIC on this pool switch, memory/CPU caps, **no checkpoints**, no integration services beyond what the base needs; `Start-VM`. (`New-VM` failed once in V0 on a folder ACL, `WRITE_DAC`;
   the rerun and the corrected grant are unconfirmed.)
5. The JIT config reaches the runner **without the network**: PowerShell Direct (`Invoke-Command -VMName`, over the VM bus) runs one command that starts the runner with the config in that one process's
   environment. Needs host Hyper-V Administrator (the broker has it) and a guest credential (the per-VM password). **Unmeasured as the broker account**; if it fails, the fallback is the existing
   one-shot listener over the switch (`spawn-jit-shim --listen`, `ListenerTransport` with the guest's static address instead of `wsb ip`). PS Direct is preferred: no listener, no network path for the secret.
6. Runner registers through the proxy, runs one job, exits; the broker sees the runner exit (PS Direct or VM heartbeat).
7. `Stop-VM -TurnOff`, `Remove-VM`, delete the child and seed; a sweep by name prefix removes orphans; the VM's lifetime is capped; the broker ends a VM when its requester disconnects (the broker design's connection-owned lifetime).

## First boot without cloud-init (Windows)

Two ways, neither measured here:
* **Unattend on the seed.** A sysprepped base reads `unattend.xml` at specialize/oobeSystem; Windows searches removable media for it (my recollection of Microsoft's answer-file search order, **not checked**: read it before building).
  Content: computer name, static IP, proxy environment, autologon of a local non-admin runner user, a `RunOnce` or scheduled task that waits for the start command. More fragile than cloud-init.
* **Bake everything into the base** and pass only per-VM values over PS Direct after boot (address via `netsh`, proxy env, start). Fewer moving parts at first boot, but the guest needs a working local admin credential at that point; PS Direct needs one anyway.
Recommend the second, to be tried first: one fewer file format, and PS Direct is needed regardless.

## The base image

CireSnave 2026-11-21 (relayed): the guest "was going to be an evaluation copy. Can't we just download that?" Candidates, from the Evaluation Center pages read 2026-11-21 (page text, not licence text):

| Image | Period | Format on the page | Notes |
|---|---|---|---|
| Windows 11 Enterprise (26H2; LTSC 2024) | "90-day evaluation"; after expiry "the PC will shut down every hour"; no product key | **ISO only** (x64, Arm64); "could take a few hours"; sign-in with a Microsoft account is mentioned | Gen2 Windows 11 wants a vTPM and Secure Boot: unmeasured for a differencing clone |
| Windows Server 2025 | "expires in 180 days"; activate online within 10 days or it shuts down; "convert your evaluation versions to retail" | ISO and **VHD** (English) | no TPM requirement known to me; a ready VHD saves the install; Server Core is smaller (unmeasured) |
| Windows 11 dev environment (Hyper-V Quick Create) | Evaluation build; the gallery feed entry is `WinDev2407Eval.HyperV.zip`, version 10.0.22621, updated 2024-07-25, 20 GB disk requirement, Secure Boot, includes Visual Studio Community 2022 (**read by the PM**, feed `go.microsoft.com/fwlink/?linkid=851584`, not by me) | zip of a Hyper-V image | EULA (`aka.ms/windowsdevelopervirtualmachineeula`) read by the PM, who reports 2(d)(v) forbids installing for use only by remote users and offering commercial hosting, one instance per virtual device, remote access limited to one licensed user: a runner guest used only through GitHub jobs may fall under "only by remote users" (PM's model reading, no attorney; **I have not read the text**) |

Consequences: the evaluation clock starts when the base is first installed, not per child, so **all children share one expiry**: build a new base well before it (a schedule, two bases overlapping), record its hash in the broker's
config. A fresh base per rebuild is also the update path. Disk: tens of GB for a prepared base with a toolchain (unmeasured); about 970 GB free (PM, 2026-11-21).

**Licence terms: not read.** The pages do not state production/CI use, rearm count, device count or guest-on-host use. The registration step (`info.microsoft.com`, via the fwlink on the page) is a form asking for personal details, so I did not
fill it in or download; the EULA ships inside the ISO. Until the EULA is read from the primary text, nothing is built on the evaluation image (PM).

## Controls before any runner trusts it (none run)

Guest connect to a public IP fails; to the host's other ports fails; UDP 53 fails; the proxy path works for `api.github.com`; a guest on switch A cannot reach switch B's guest; the JIT config appears in no file, argument, log or
guest process listing (PS Direct); a killed broker leaves no VM (orphan sweep); `Stop-VM -TurnOff` mid-job leaves no differencing disk. Known-bad controls: a switch with forwarding enabled, and a guest with a DNS server set, must each FAIL.

## What this changes in the repo

`spawn adapter` keeps refusing a `windows-sandbox` group (it has no network policy); the stale "board 322" text is corrected to say so. A new launcher id (`hyperv-vm`) behind the broker, a `VmBackend: RunnerBackend`, and the proxy are new
work (ROADMAP rows 38 to 44). The broker (`spawn-sandbox-broker`, V7 paste) is unrun: CireSnave's elevated runs via the PM.

## Windows Server 2025 Core as the guest (assessment on paper, 2026-11-21; pages read, nothing downloaded)

CireSnave asked: *"What about using Windows Server 2025 Core for the image?"* Sources are Microsoft Learn, GitHub, Rust, Git for Windows, Intel and AMD pages read that day; where a page was unreachable it says so.

* **Runner:** GitHub's list names Windows Server 2016, 2019 and 2022 only; 2025 and Core are not stated. The runner targets net8.0 self-contained, and .NET 8's supported-OS table lists Windows Server 2025 and Windows Server Core 2025: supported by inference.
* **GitHub's own hosted images** build from `MicrosoftWindowsServer:WindowsServer:2025-Datacenter-g2` (runner-images `locals.windows.pkr.hcl`) with Visual Studio Enterprise, browsers and WSL2: I read that SKU as Desktop Experience, not Core (SKU naming from memory, unchecked). They end with `Sysprep.exe /oobe /generalize /mode:vm`.
* **Jobs on Core:** Visual Studio Build Tools 2022 "Also installs on the Server Core option for Windows Server Core 2022, Windows Server 2025" (needs .NET Framework 4.8, WebView2, admin). Rust x86_64-pc-windows-msvc: "Windows Server 2016 or higher is required for server installs", Core not mentioned. Git for Windows: Windows 10 or later, Server/Core not mentioned. PowerShell is available on Core. **Intel MPI** lists Windows Server 2019 and 2022, not 2025 or Core; **AOCL 6.0**'s validation matrix lists Windows 10 and 11 Pro only. So the aocl job is not covered by vendor support on Server 2025 Core.
* **Core lacks** the desktop shell, Explorer, MMC, Perfmon, Event Viewer, Control Panel, Edge, audio and OOBE; the App Compatibility Feature on Demand (a separate ISO, one restart) adds Explorer, MMC, Perfmon and similar tools. A job needing a browser or desktop session will not run on Core.
* **Size and rearm:** Core is "~4 GB smaller" than Desktop Experience; 32 GB is the "absolute minimum" disk; 2 GB RAM for Core. VHD/ISO sizes and Core boot time: not stated, unmeasured. Evaluation: 180 days, activate online within 10 days or shut down (a guest behind the proxy would need activation hosts as an exception: unmeasured); convertible to retail but "you can't convert Desktop Experience evaluation versions to Core retail versions". Rearm count and extending 180 days: not stated on any page reached. Differencing-disk and clone clock/licence behaviour: not stated anywhere (inference: children share the base's install date).
* **Licence:** the Server evaluation licence text was not reachable (registration form). `DISM /online /Set-Edition:<edition> /GetEula:C:\license.rtf` on an installed evaluation writes it out. CireSnave 2026-11-21 (relayed): *"The GitHub runners don't serve remote users. I don't think the remote users concern applies even if we used the full Windows 11 image."* The "only by remote users" item is therefore not treated as a blocker; it stays quoted above for the record. One instance per virtual device and the rearm/expiry terms remain open until the EULA text is read.
