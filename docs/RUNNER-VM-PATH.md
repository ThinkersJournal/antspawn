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

## Building the Core base from the evaluation ISO (design, 2026-11-21; nothing built)

Why: the Evaluation Center VHDX is Desktop Experience (CireSnave: "It boots right into the full desktop version"; guest KVP: Windows Server 2025 Datacenter Evaluation 10.0.26100). A Desktop edition cannot be converted to Core, so Core needs the ISO (CireSnave offered to download it into `C:\vmimages`).

* **Route 1 (recommended, no elevation): let Setup do the install inside a throwaway Gen2 VM.** As `cires` (Hyper-V Administrator): `New-VHD` a 64 GB dynamic VHDX, `New-VM -Generation 2`, attach the ISO as DVD and a second small ISO carrying `Autounattend.xml`, no NIC (or the no-route switch), boot. Setup reads the answer file from removable media. Whether the seed ISO can be built without elevation (IMAPI2 as `cires`, or `oscdimg`) is **unmeasured**; a second small VHDX holding the file is the fallback (Setup scans all drives).
* **Route 2 (host-side apply): needs elevation.** `Mount-DiskImage` the ISO, `dism /Get-WimInfo /WimFile:<iso>\sources\install.wim` to list indexes, create and partition a VHDX (EFI + C:), `Mount-VHD`, `dism /Apply-Image /Index:<Core> /ApplyDir:`, `bcdboot`. DISM apply, partitioning and `Mount-VHD` all need an elevated token: CireSnave's, via the PM. Not needed if Route 1 works.
* **Which index:** read it, do not assume: `dism /Get-WimInfo` names each edition ("... SERVERDATACENTERCORE" vs "... SERVERDATACENTER" is my recollection of the naming, not checked). Take the Datacenter Evaluation Core entry.
* **Answer file (`Autounattend.xml`):** pass `windowsPE`: `DiskConfiguration` (GPT: 100 MB ESP, MSR, rest C:), `ImageInstall/OSImage` selecting the Core image by `/IMAGE/NAME`, `UserData` with `AcceptEula` true (the EULA read first, by a human: the Licence step of `scripts/vm-pilot.ps1`, or the ISO's own text) and no key (evaluation). Pass `oobeSystem`: `UserAccounts/AdministratorPassword`, `HideEULAPage`/`SkipMachineOOBE`. **The password sits in the answer file in plain text (base64 is not protection): the file lives only on the seed media, deleted with the build VM, generated from `ANTSPAWN_PILOT_PW` at build time, never committed or printed.** Setup also copies it to `C:\Windows\Panther`: delete that copy (and `unattend.xml` there) as the last step in the base.
* **Sysprep or not:** a differencing child of an unsprepped base shares the SID, computer name and (evaluation) install date; the VM has no domain and no network except the proxy, so SID sharing matters little; the name is set per job over PS Direct. `sysprep /generalize` would re-arm OOBE per child (the answer file then must also drive `specialize`) and counts against the evaluation rearm limit (not read). Recommend: no sysprep for the pilot; revisit when the licence text is read.
* **After install, as the guest Administrator over PS Direct:** `Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' InstallationType` must read `Server Core` before anything else is built; then the steps of the Desktop pilot, labelled Core.
* **Elevated, if any:** only Route 2 (above) and, as always, setting a host address on a pool switch. Route 1 uses `cires` alone.

**Recorded 2026-11-21: CireSnave read the Microsoft EULA and approved acceptance, 2026-11-21. His words (relayed by the PM): "I've seen Microsoft's EULA.  It can be set to yes."** `AcceptEula` true is authorised for the Windows Server 2025 evaluation images, including a runner image built from this ISO; it covers that evaluation only, not other Microsoft products. (Before his reading, the lane had read only the on-image stub, which points at aka.ms/useterms: `C:\Windows\System32\Licenses
eutral\Eval\ServerDatacenterEval\license.rtf`, EULAID Sept2020_DCSTD_EN-US, kept at `C:\vmimages\pilot\eval-license.rtf`, and the English OEM terms PDF; the evaluation's own terms were not reachable by the lane.)

## Model files for the corpus job (design, 2026-11-21; nothing built; board 458 folded into 467)

Requirement (PM; CireSnave 2026-11-21: *"I agree with both making c:\models read only for the VM and with folding 458 into 467 since they're the same type of work."*): ml-convertant `corpus.yml` (label `corpus`) reads about 3.4 GB of real model files from `C:\Models`. A guest on an Internal switch cannot see the host path, and must stay without a route off the host. The VM gets the models **read-only**. The host folder's own ACL (Modify for Authenticated Users, inherited) is **not touched**.

| Option | How | Read-only | Cost | Verdict |
|---|---|---|---|---|
| **A. Read-only data disk (recommended)** | A VHDX holding the models, built once, kept read-only on the host; each job VM gets a **differencing child** of it attached as a second disk, mounted at `C:\Models` in the guest | the parent file is read-only; the child absorbs any write and is deleted with the VM; the guest disk can also be set read-only in the guest | no per-job copy: a child is about 1 s (measured on a 64 MB disk); storage 3.4 GB once plus the child; refresh = rebuild the VHDX when the models change | no network path, no share, no elevation |
| B. SMB share over the switch | host `New-SmbShare` on the host address | share ACL can be read-only | opens TCP 445 and a credential on the pool interface, against the rule that only the proxy port is reachable; `New-SmbShare` is elevated | rejected |
| C. Per-job copy over PowerShell Direct | `Copy-Item -ToSession` of 3.4 GB into each VM | copy is private to the VM | 3.4 GB per job; rate **unmeasured** (a measured number must come before this is offered) | fallback only |

Option A details. **Build with no elevation:** boot a build VM with a blank VHDX, copy the models in over PowerShell Direct, format and label the volume in the guest, stop the VM; the VHDX is the data base (record its sha256 and the manifest of the source files beside it in the broker's config). **At job time:** `New-VHD -Differencing` from that parent, `Add-VMHardDiskDrive`, and a one-time guest setting that mounts the volume at `C:\Models` (`Add-PartitionAccessPath`), so the workflow path does not change. The guest user running the job is not an administrator, so it cannot re-mount the disk read-write; the child is destroyed with the VM either way. **What it does not protect:** a stale copy (the host's `C:\Models` may change after the build: a manifest check at job start compares it, `spawn adapter --check` reports the age); and the models' own licences (not read here). Open items: the exact filesystem (NTFS) and whether any job needs more than 3.4 GB; the guest-side read-only flag; the rate of a rebuild. The third repo for the runner automation, after aocl and WebHostAnt.

## Visual Studio Build Tools terms: accepted for the runner VM image (2026-11-21)

CireSnave 2026-11-21 (board 486, option A), verbatim: *"I am a solo developer and even if we consider ThinkersJournal or Evans Laboratories, they have one person working for them (me) and only 2 PCs (my laptop and desktop)."* The Build Tools supplement is accepted for the runner VM image. **Conditions (PM): the baked image never leaves this laptop; no copy to another machine and no registry; build devices are machines we own.**
Texts read (lane's reading, not legal advice; saved with sha256 under `C:\vmimages\installers\`, which is outside the repo): *VS 2022 Diagnostic Build Tools* (`...Agent-License_Update-March-2024_EN.docx`, sha256 2f66b86a...7bdc): "This software is licensed as a supplement to Visual Studio Community, Visual Studio Professional, and Visual Studio Enterprise ... Use of the software requires a valid license to one of the Visual Studio Products."; "You may copy and install files from the software onto your build devices, including physical devices and virtual machines or containers on those machines, whether on-premises or remote machines that are owned by you ..."; and *VS 2022/2026 Community*: "If you are an individual working on your own applications, either to sell or for any other purpose, you may use the software to develop and test those applications."; an "enterprise" is more than 250 PCs or users or one million US dollars in annual revenue. Both texts bar running components "on third party devices" and transferring the software to a third party.

## Reproducible install on Server Core: the measured recipe and the bases (2026-11-21)

Everything below ran as `cires` (Hyper-V Administrator, no elevation) in a Server 2025 Datacenter **Core** guest (26100.32230) on an Internal switch with no host address: **no network in the guest**. Scripts: `scripts/vm-core-build.ps1` (base from the ISO), `scripts/vm-pilot.ps1` (child, boot, destroy), `scripts/vm-pilot-install.ps1` (install, test, runner, bake). All run under `with-secret ANTSPAWN_PILOT_PW` with `powershell.exe` (the Hyper-V and CIM cmdlets fail in `pwsh` here).

**Inputs, kept in `C:\vmimages\installers` (outside the repo; sha256 recorded):** ISO `26100.32230...SERVER_EVAL_x64FRE_en-us.iso` (8,152,356,864 bytes, sha256 7B052573...FD51; no published checksum); `vs_BuildTools.exe` 17.14.37906.3 (Authenticode valid) and its layout `bt-layout` (1,329 MB, `--add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.26100`: one SDK, 10.0.26100; created by `vs_BuildTools.exe --layout`, no install, non-elevated); `vc_redist.x64.exe` 14.44.35211 (valid, sha256 CC0FF0EB...713B); `Git-2.56.1-64-bit.exe` (valid; sha256 C330C72E...5A559 = the release page); `rust-1.100.0-x86_64-pc-windows-msvc.msi` (sha256 568293ab...3a9b = static.rust-lang.org; **not** Authenticode-signed); `actions-runner-win-x64-2.338.1.zip` (sha256 478f533a...7ff9 and 104,210,469 bytes = the pin in `src/adapter/runner_pin.rs` = the GitHub release digest; `Runner.Listener.exe` and `Runner.Worker.exe` are **not** Authenticode-signed, the bundled `node.exe` is, by the OpenJS Foundation).

| Step | How | Measured |
|---|---|---|
| Core base from the ISO | `vm-core-build.ps1`: Setup inside a Gen2 VM, `Autounattend.xml` on a UDF seed ISO (IMAPI2, no elevation), WIM index 3 (`SERVERDATACENTERCORE`, read from the WIM's XML: `dism` needs elevation even to list), a key tap to pass "Press any key" | 5.7 min Setup + first boot; `InstallationType` = `Server Core`; base 9.10 GB on disk; C: 8.30 GB used |
| Child of a base | `New-VHD -Differencing` + `New-VM -Generation 2` (the VM is a differencing child; the base is marked read-only) | first heartbeat 9.9 to 21.3 s (several samples, this host) |
| Copy into the guest | `Copy-Item -ToSession` (PowerShell Direct) | **5.0 to 5.4 MB/s**: 1.7 GB of installers 397 s; avoid with a baked base |
| Build Tools, offline | `vs_BuildTools.exe --noweb --quiet --wait --norestart --nocache --installPath C:\BuildTools --add ...` from the layout | 246 s, +4.2 GB; **two fixes** below |
| VC++ redistributable | `vc_redist.x64.exe /install /quiet /norestart` | 5 s |
| Git for Windows | `/VERYSILENT /NORESTART /SUPPRESSMSGBOXES /NOCANCEL /SP-` | 35 s |
| Rust | `msiexec /i rust-1.100.0-...msi /qn /norestart` (installs under `C:\Program Files\Rust stable MSVC 1.100`) | 217 s |
| Runner | the verified zip extracted to `C:\runner` (no `config.cmd` run, no credentials) | `Runner.Listener --version` = 2.338.1, exit 0 |
| Test | `cargo test --locked --offline -p aocl-scalapack` with AOCL copied in, `AOCL_ROOT=C:\AOCL-Windows`, no `LIB`/`INCLUDE` | passes, 61 s cold build |

**Offline Build Tools fixes (both measured):** (1) the bootstrapper stops with `0x80131509` (exit 5003): the signing chain of `vs_installer.opc` lacks the intermediate *Microsoft Windows Code Signing PCA 2024* (the layout's `certificates` folder holds only roots, already in Windows). Build the chain on the host (online), send that one intermediate (thumbprint D30F05F637E605239C0070D1EA9860D434AC2A94) to the guest's `LocalMachine\CA` store, install, **then remove it again**. (2) `setup.exe install` rejects `--lang` (exit 87): omit it (the layout's `Response.json` carries the language).
**VC++ runtime:** Build Tools does not put `vcruntime140`, `vcruntime140_1`, `msvcp140`, `vcomp140` in System32 (it keeps copies only under `C:\BuildTools\VC\Redist\MSVC\...`); stock Core lacks them; with `vc_redist` installed, AOCL's DLLs load. Keep `vc_redist` in the image.
**Not in any image: the guest-only changes made while debugging.** The offline-install intermediate certificate (removed from `LocalMachine\CA`, read back absent) and the per-user `WinTrust` "publisher revocation off" value (removed, read back absent) are out of every base from `core-runner-base.vhdx` on; the Microsoft roots the layout's `certificates` folder offers were already in the stock Windows root store (15 roots after the install), so nothing was added there. `vm-pilot-install.ps1 -Step Bake` removes both before baking. The AOCL tree, Intel MPI and the five Intel runtime DLLs are baked only in `core-vendor-base.vhdx` (board 487 ruling A, below).

**Bases in `C:\vmimages\pilot` (all read-only, stay on this laptop; no copy, no registry):**
| File | Contents | Size on disk |
|---|---|---|
| `core-base.vhdx` | clean Server 2025 Datacenter Core evaluation, 26100.32230 | 9.10 GB (C: 8.30 GB used) |
| `core-runner-base.vhdx` | + Build Tools (MSVC 14.44, SDK 10.0.26100), vc_redist, Git 2.56.1, Rust 1.100.0, runner 2.338.1; certificate and WinTrust tweaks **absent** | 16.69 GB (C: 15.29 GB used) |
| `core-vendor-base.vhdx` | `core-runner-base` + the vendor layer below (AOCL tree, Intel MPI, five runtime DLLs, licence texts, sha256 manifest) | 17.47 GB (C: 16.24 GB used) |
The two superseded bases (`core-tools-base`, `core-tools-runner-base`; the offline-install intermediate certificate was still in their CA store) were deleted on the PM's word, read back absent.
A child of the runner base boots to a heartbeat in 12.4 s (one sample); a `cargo` hello built in 4.2 s on a child of the earlier `core-tools-base` (same toolchain). The evaluation clock starts at the base's install: all children share its expiry (rearm count 1 was read on the Desktop image).

**Vendor layer (board 487, CireSnave 2026-11-21, verbatim: "487: A").** `vm-pilot-install.ps1 -Step Vendor` (then `-Step Bake -KeepVendor -Out core-vendor-base.vhdx`) puts, on top of `core-runner-base`: the host's AOCL tree (`C:\Program Files\AMD\AOCL-Windows`, 605 files, 863 MB) at `C:\AOCL-Windows`; Intel MPI 2021.18 (`...\oneAPI\mpi\2021.18`, 70 MB) at `C:\IntelMPI`; and five runtime DLLs at `C:\oneapi` (impi.dll from the MPI tree; libifcoremd.dll and libiomp5md.dll from `compiler\2026.1\bin`; libmmd.dll and svml_dispmd.dll from `advisor\2026.0\bin64`). The licence texts are in the image at `C:\docs\licences` and on this laptop in `C:\vmimages\installers\vendor-licences` (not committed here: AMD's agreement forbids distribution, so the repo records readings, not copies). `C:\docs\vendor-MANIFEST-sha256.txt` (885 lines: sha256, size, guest path, host source path of every baked file and licence; the host copy's sha256 is 40e8c371...65a7) was read back in a fresh child: 885 files, 0 mismatches. The image stays on this laptop, no copy, no registry, read-only.
Measured from a fresh child of `core-vendor-base` (no host mounts: one drive `C:`, 0 SMB mappings; the aocl source arrives as a tar over PowerShell Direct): `cargo test --locked --offline -p aocl-scalapack` passes (1 test, 85 s cold build); `mpiexec -n 1` and `mpiexec -localonly -n 1` launch a command and exit 0 (1 s / 0.2 s; Hydra needs no network for one local rank); `impi_info` runs. The aocl test never calls `MPI_Init`, so a one-rank MPI *program* is not yet exercised. `vc_redist` stays in the image (the four VC++ DLLs are in System32).
**Licences read (the lane's reading, not legal advice):** the AOCL package ships an "EVALUATION AND OBJECT CODE DISTRIBUTION AGREEMENT" (internal use solely for evaluating; no distribution or transfer). AMD's current web page (`https://www.amd.com/en/developer/aocl/eula/aocl-eula.html`, fetched 2026-11-21 by curl, saved as `AMD-AOCL-EULA-web-2026-11-21.txt`, sha256 28be2934...273c) is a per-library compilation: BLIS, libFlame, ScaLAPACK, AOCL-Sparse and LibM carry BSD-style terms, while the evaluation-style text appears only for AOCL-libMEM and others, and separate AMD agreements for the Secure RNG and RNG libraries. So it differs from the single installed agreement: more permissive for the libraries this image uses. Intel MPI: Intel Simplified Software License (Oct 2022); the compiler and Advisor runtimes: Intel EULA for Developer Tools (Aug 2024). Not established: whether libmmd.dll and svml_dispmd.dll, which came from the Advisor folder, are listed redistributables (that folder's redist.txt lists only annotation headers).


## The guest channel and the JIT hand-off (ROADMAP row 40; measured 2026-11-22, this laptop)

**Credential (CireSnave board 488, "A"):** the baked image has one fixed Administrator password, kept as a DPAPI user-scope file `%LOCALAPPDATA%\antspawn\guest-credential.dpapi` (`scripts/store-guest-credential.ps1`; rotating it means rebaking). Every PowerShell Direct script builds its `PSCredential` from that file inside its own process; the password is never in a script, an argument, a log or a status file. `spawn adapter --check` exits 2 when a `hyperv-vm` group has no readable file.

**Readiness:** the heartbeat is OK a few seconds after start but PowerShell Direct answers later (about 12 s after start in the runs here; the first session sent too early broke after 30 s). `wait_ready` therefore waits for the guest to answer a command.

**JIT hand-off (`VmTransport`):** the config goes to a host `powershell.exe` on **stdin** (never an argument or file), then into the guest as an `Invoke-Command -ArgumentList` value over the VM bus, where a `ProcessStartInfo` sets `ACTIONS_RUNNER_INPUT_JITCONFIG` on the one `cmd /c run.cmd` child and the guest variable is cleared at once. The child outlives the session (measured). An error text has the config replaced by `<redacted>`; a config that is empty, over 64 KiB or not base64 is refused before it leaves the host. The scripts are passed with `-EncodedCommand`: a multi-line script on stdin to `-Command -` was measured to end at once with exit 0 and no output.

**Runner-end wait:** polls `Runner.Listener` in the guest over PowerShell Direct (appears, then gone, or the VM is off).

**Measured with `examples/vm_pilot.rs` (one run, a FAKE config, no network, no GitHub):** ready 12 s after start; delivery 5 s; the real `Runner.Listener` started and ended (it rejects the made-up config) and the wait reported "runner ended" 14 s later; a search of `C:\runner`, `C:\Users`, `C:\ProgramData` and `C:\Windows\Temp` in the guest found 0 files holding the config, and the same search found the planted control file (1); the host output held it 0 times. **Not measured:** a real registration (needs the proxy path and the `antspawn-proof` repo, with CireSnave's Hello), guest-side PowerShell logging policy beyond this image's defaults.
