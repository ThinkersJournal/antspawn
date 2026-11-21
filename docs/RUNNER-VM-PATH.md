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
