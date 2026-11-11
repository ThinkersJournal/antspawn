<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# A VM broker: any runner may ask for a VM, none may touch Hyper-V (options note, revision 3)

Tasked by the PM 2026-11-07 (`[TASK] sandbox-broker-options`, revised by `[TASK] vm-broker-revision`). **Design and options only: nothing here is built, nothing is decided.**
Written at main `3df0a14`; S2 to S4 (`session_proto`, the host pipe, the owner watcher) are open PRs #72, #74, #75, not on main. Revision 3 (PM `[TASK] windows-guest-profile`) corrects revision 2's claim that Windows guests are out of scope. Revision 1 (a broker for Windows Sandbox only) is in this
file's history; it is kept below as the **Windows Sandbox path**, shorter.

CireSnave, 2026-11-07, verbatim (relayed by the PM), first message: *"Aren't those accounts the runners themselves?  Should they have the ability to set up their own sandbox or should some other account set them up and they run inside them?"*
and the second: *"I think you may be overthinking this.  If the broker allows any runner to set up a secure VM which the runner can then SSH into and populate with whatever it wants to run there and the runner can then have the broker kill that VM they just had it create, as long as the runner shutting down automatically signals the broker to kill any VMs the runner created, where is the potential harm in any runner being able to launch a VM?  I mean, they could launch VMs until they overloaded the system, but it shouldn't be hard for the broker to watch for and prevent that sort of attack."*
Also from the PM: `gh-runner` holds Hyper-V Administrators only so a PersistAnt test (`kv.durability.machine`: create a Hyper-V Linux guest, SSH in, cut its power, restart, check) can run; the goal is to remove that membership from every runner account.

**Short answer to "where is the harm":** he is right that a runner launching a VM is not the harm, *if* the VM is the only thing it gets. The harm is everything a Hyper-V VM can be wired to besides itself: the host's network
interfaces, the LAN, a host folder, a passed-through device, an image the runner wrote, a handle to the Hyper-V WMI/PowerShell interface itself (which is what the group grants: full control of Hyper-V on this box).
So the design is: the runner gets a *name for a VM and an SSH address*, never Hyper-V; every wiring decision is the broker's and fixed. Resource abuse is the smaller risk and is capped (below).

## What is measured, and what is not

Measured today (2026-11-07, read-only commands as `cires`, a non-elevated token that carries `Hyper-V Administrators`; this is a read, no VM was created and nothing was changed):
- Windows 11 Pro build 26300; 63.2 GB RAM, 32 logical CPUs (so caps below are sized against real numbers).
- Hyper-V is running: `vmms`, `vmcompute`, `hns` services are Running; the cmdlets `Get-VM`, `New-VM`, `Get-VMSwitch`, `Get-NetFirewallHyperVVMSetting` exist; `Get-VM` works non-elevated and lists **no VMs**.
- Two switches exist, both `Internal`: `Default Switch` and `WSL (Hyper-V firewall)`. IPv4 forwarding is **Disabled** on both vEthernet interfaces; `Get-NetNat` lists nothing (the Default Switch's sharing is ICS, not a NAT object).
- The Hyper-V firewall has **one registered VM creator: WSL** (`{40E0AC32-...}`, default inbound Block, outbound Allow). So the per-VM Hyper-V firewall is shown here only for WSL.
- OpenSSH client at `C:\Program Files\OpenSSH\ssh.exe`.
- From antspawn#73 (2026-11-06): as `antspawn-run` (non-interactive) `wsb list`/`wsb start` return `Cancelled`; `BUILTIN\Hyper-V Administrators` holds exactly `cires` and `gh-runner`. Windows Sandbox runs one VM at a time (`CO_E_APPSINGLEUSE`).
- A named pipe with a protected one-SID DACL, remote clients rejected, works between processes of **one user** (S3 tests). The shim's `--pipe --allow-sid` is built the same way and tested only as the current user.

**Measured 2026-11-08, V0 first probe** (`scripts/broker-v0.ps1`, run by CireSnave; ref: main `15586d6` plus the two fixes of #82; one run; the task ran as `antspawn-v0-broker`, a local user in `Hyper-V Administrators` with the batch-logon right, token **not elevated**; relayed through the PM, not read by the lane):
- A non-elevated Hyper-V Administrator **can** `Get-VM` (1253 ms, 0 VMs), `Get-VMHost`, **create an Internal switch** (`New-VMSwitch`, 7277 ms), and create a dynamic 64 MB VHD and a differencing child (about 1.1 s each) and delete them. A new switch had IPv4 forwarding **Disabled** and `Get-NetNat` listed nothing. Total 15 s.
- **The broker cannot set the host address on the switch's vEthernet** (`New-NetIPAddress`: access denied). That is the elevated setup's job, as designed: the address is set once, by hand or by the setup script, never by the broker.
- **`New-VM` (generation 2, with the child disk) failed**: `failed to set folder permission for ...\results: Access is denied (0x80070005)`. The folder gave the broker `Modify`. The reading (the PM's, **not yet confirmed**; the rerun with the corrected grant shows it) is that Hyper-V grants the new VM's per-VM SID on the disk's folder as the caller, which needs the right to change that folder's ACL (`WRITE_DAC`, which `Modify` lacks). **Design consequence, to be confirmed by the rerun:** the broker needs **full control of its own disk directory and of nothing else** (the base images stay read-only to it; `broker-v0.ps1` now grants `results\` full control and reads the ACE back). Every later VM step of that probe could not run, so start, stop, delete, the seed ISO and guest steps are **unmeasured**.

**Measured 2026-11-08, V0 second probe** (same script after the folder grant became full control; one run; relayed by the PM from CireSnave's `probe.txt`, not read by the lane): `New-VM` (generation 2) failed again, now with `Sequence contains more than one element`, so **the folder-permission question is still unanswered**. The PM's reading, not confirmed: Hyper-V allows two switches with one name, the first run had left `antspawn-v0-sw`, the second run's `New-VMSwitch` then made another (it printed `Internal | Internal`), and `New-VM -SwitchName` resolved two objects. Consequence for the broker: **it must never rely on a name being unique to the switch it just made**; it creates, then refers to the switch by its `Id`, and a pre-existing object of the same name is a refusal, not a second create. `broker-v0.ps1` now refuses to start the probe (and `-Apply` refuses to register anything) while a switch named `antspawn-v0-sw` or a VM named `antspawn-v0-vm` exists. Also in that run `New-NetIPAddress` on the switch's vEthernet by the **non-elevated broker succeeded**, where the first run said access denied. **Unexplained; do not rely on either result.** Candidate readings (adapter not yet ready in run 1, or two interfaces with one alias in run 2) are untested; the probe now also records how many adapters and switches answer to the name before that step, and a clean single-switch rerun is what answers it.

**Measured 2026-11-08, V0 clean run** (third run, one switch, 24 s; `scripts/broker-v0.ps1` at `a41d29d`'s grants; as `antspawn-v0-broker`: Hyper-V Administrators, batch logon, **not elevated**, full control of its own `results` directory only; run by CireSnave, relayed by the PM from `probe.txt`, **not read by the lane**; one clean run plus two earlier runs that failed for the reasons above):

| Step | Result |
|---|---|
| `New-VMSwitch` Internal | OK, 3708 ms |
| new switch IPv4 forwarding / `Get-NetNat` count | Disabled / 0 |
| broker sets the host address on the switch's vEthernet | **FAIL, Access is denied.** So run 2's OK was an artifact of the duplicate switch; **the elevated setup sets the address, as designed** |
| `New-VHD` dynamic parent / differencing child | 1090 ms / 1113 ms |
| `New-VM` generation 2 on the child disk | OK, 2361 ms (the first run's refusal was `Modify` lacking `WRITE_DAC` on the disk directory) |
| static memory, `ExposeVirtualizationExtensions` false, integration services all but Heartbeat off, no checkpoints | OK |
| `Start-VM` / `Stop-VM -TurnOff` / `Start-VM` again, same VM and disk / `Stop-VM -TurnOff` again | 7670 ms / 536 ms / 3867 ms / 208 ms |
| `Remove-VM` / delete the disks | 103 ms / 39 ms |

So a **non-elevated Hyper-V Administrator can run the whole lifecycle including the power cut**, provided it has full control of its own disk directory; that answers unmeasured item 2 below for an empty VM on a clean switch. The design consequence stands: the broker's account needs **full control of its own disk directory and nothing wider**, the base images stay read-only to it, and the host address is the elevated step's. Still unmeasured here: a real OS booting from the disk (these VMs have no OS), the seed ISO, the pipe between two accounts, TCP from a client, a guest probing the host.

**Not measured, and each can change the design:**
1. **A pipe between two different accounts** (runner to broker), for any feature.
2. **Whether a non-interactive service account in Hyper-V Administrators can create, start, stop and delete a VM and a switch.** `Get-VM` as an interactive, non-elevated member is the only read taken. (The `wsb` cancellation is Windows Sandbox's packaged-app activation; nothing says Hyper-V VMs share that failure, nothing says they don't.)
3. **Whether the per-VM Hyper-V firewall can be applied to a generic (non-WSL) VM.** Only WSL is a registered creator here. Do not rely on it; the design below does not.
4. **Guest reachability from a restricted account**: can `ghr-*`/`antspawn-run` open a TCP connection to a vEthernet address at all (their outbound Windows Firewall rules, set by `account-setup.ps1`, block outbound for `antspawn-run`)?
5. A Linux guest image: its boot time, whether a seed ISO gives it a static address and a key, differencing-disk growth, `Stop-VM -TurnOff` behaviour for the durability test.
6. Hyper-V Administrators' effect on the `account` isolation rows, and whether any runner could today start a VM (they cannot be assumed unable: `gh-runner` is in the group and runs workflow code).

## The design: a general VM broker

```
 runner account (any in group `antspawn-vm-clients`)          broker (one dedicated account, the ONLY Hyper-V Administrators member)
   pipe \\.\pipe\antspawn-vm   vm.create{image,profile,pubkey} -> picks a free pool switch, makes a differencing disk + seed ISO,
                               <- {vm id, ip, port}              creates the VM from a fixed template (does NOT start it)
   ssh -i key user@ip  (runner -> guest, over the pool switch; the runner populates and runs whatever it wants)
   vm.stop{force} / vm.start / vm.delete / vm.status / vm.list(own)
   (runner exits or the pipe closes)  ------------------------>  broker destroys every VM that connection created
```

### Lifetime: a VM belongs to a connection
- Every VM is owned by the **pipe connection** that created it. Connection closed (runner exit, crash, `close`) means: `Stop-VM -TurnOff`, `Remove-VM`, delete the differencing disk and the seed ISO, release the pool switch. S4's `peer_alive` is the mechanism, run from a watcher thread, so a long-running broker call does not delay it.
- Hard ceiling per VM (`max_lifetime`, default hours, set in the broker's config); a connection that stays open does not outlive it.
- **Orphan cleanup by name prefix:** every VM, disk and ISO is named `antspawn-<id>`. On broker start (and every few minutes) it removes everything with that prefix that no live connection owns. A broker restart therefore kills all VMs, consistent with "VM lifetime = connection".
- A runner that must keep a VM across its own restart is not supported; that would need a lease, and nothing asks for it.

### What the broker holds (and the runner does not)
- Hyper-V Administrators: **one dedicated account, `antspawn-broker`**, non-interactive, task at boot. Every runner account leaves the group, including `gh-runner`. That is the goal the PM stated and it is reachable only if the broker works as such an account (unmeasured item 2).
- Elevated setup (CireSnave's, once, never by antspawn): creates the account and group `antspawn-vm-clients`, the pool of Internal switches with fixed host-side addresses, the read-only base images, the broker's directory ACLs, firewall rules, the boot task. The broker creates none of these.

### Controls (each a test with a known-bad request that must FAIL a broker that allows it)
| Control | How | Status |
|---|---|---|
| **No host paths in a request** | A request carries `image` (a name from the fixed set), `profile` (a name), `pubkey` (bytes, size cap, one line, validated as an OpenSSH public key). No path, no XML, no script. | design |
| **Guest has no route to the LAN or the internet** | NIC attaches only to a **pre-created Internal switch from the pool, with no NAT and IPv4 forwarding Disabled** (forwarding is Disabled on both existing vEthernet interfaces, measured; a new switch's default is unmeasured). The guest can reach only the switch's host-side address. | design; switch default unmeasured |
| **Guest cannot use the host's services** | The host-side address is reachable from the guest by construction, so host Windows Firewall rules (elevated setup) block inbound on the pool interfaces except what the design needs (nothing). Needs measuring: a guest probing `host:445`, `host:22`, `host:135`. | design; unmeasured |
| **Guests cannot reach each other** | One switch per VM from the pool (no shared switch). | design |
| **Per-VM Hyper-V firewall** | Not used as the control: only WSL is a registered creator here (measured). Revisit if a creator can be registered for our VMs. | unmeasured |
| **Integration services off** | `Disable-VMIntegrationService` for Guest Service Interface, KVP Exchange (it leaks host name to the guest), Shutdown (the broker powers off), Time sync, VSS; Heartbeat may stay. | design |
| **No shared folders, no device passthrough, no GPU** | None of these exist in the template; the template never calls `Add-VMGpuPartitionAdapter`, `Add-VMAssignableDevice`, or passes a physical disk. The request cannot name them. | design |
| **Nested virtualization off** | `Set-VMProcessor -ExposeVirtualizationExtensions $false` explicitly, asserted by the test. | design |
| **Read-only base, per-VM differencing disk, deleted at kill** | Base VHDXs are written only by setup; the broker account has read. The broker checks a recorded hash of each base at start (cost: hashing a multi-GB file; a size+mtime cache is weaker and is a choice). The differencing disk and seed ISO live under the broker's own directory and are deleted on kill. | design |
| **No checkpoints, no dynamic memory, Secure Boot template for Linux** | Static memory makes the total computable. | design |
| **Caps** | count per account and in total; memory, vCPU and disk (the base's virtual size bounds differencing growth; the broker also refuses to create when host free space is below a floor) from named profiles, not free numbers; total lifetime per VM; creates per minute per account. On this host: 63.2 GB RAM and 32 logical CPUs; the sizing is a CireSnave/PM choice, a default such as 4 VMs total, 4 GB and 2 vCPU each is a number, not a measurement. | design |
| **Client identity** | DACL names the group; the broker also reads each connection's SID and enforces per-account caps from it. | design |
| **A request flood** | One request is read at a time per connection, a frame cap and read timeout (the S2 rules), a global in-flight create limit, refusal at the cap with "busy". | design |
| **Runner A reaching runner B's VM** | A can open TCP to B's guest address if the firewall allows the whole pool range; B's **key** is what stops a login. Narrower needs per-SID firewall rules created at runtime (the broker is not admin), so it is not promised. | design, residue stated |

Residue, stated plainly: a guest can attack the **host's hypervisor surface** (a VM escape) and the host-side address's listening services; the first is Hyper-V's, the second is the firewall line above and must be measured.

### How the runner gets a VM and SSHes in
`pubkey` goes into a per-VM seed ISO (cloud-init NoCloud: user, key, a static address derived from the pool switch's subnet, hostname). The broker returns `{id, ip, port, user}`. The runner connects with its own `ssh` as itself (the restricted account's outbound firewall must allow the pool range: unmeasured item 4, and `antspawn-run`'s is currently "block all outbound"). The broker never sees the runner's private key.

### The Linux power-cut test (`kv.durability.machine`): operations needed
| Step | Operation | Broker call |
|---|---|---|
| create | `New-VM` gen2 from the template, differencing disk on the Linux base, seed ISO, pool switch, caps checked | `vm.create` |
| start, address | `Start-VM`; wait for the guest's port 22 on the known static address (no KVP, it is off) | `create` does NOT start the VM or wait for the guest: the caller sends `start`, then polls port 22 itself (`broker_client::wait_ssh`) |
| write and sync | the runner, over SSH | none |
| power-cut | `Stop-VM -TurnOff` (the guest sees power removal; the **host's** page cache and VHDX flush behaviour are not the guest's: this tests guest-visible durability under a hypervisor power-off, not a host power loss; said so the PersistAnt side can word its claim) | `vm.stop{force:true}` |
| restart | `Start-VM` on the **same** VM and differencing disk (the disk must survive a stop) | `vm.start` |
| check | SSH again | none |
| delete | `Remove-VM`, delete disk and ISO, release switch | `vm.delete` or the connection closing |
Five broker verbs (`create`, `start`, `stop{force}`, `delete`, `status`/`list` own), and the connection owning the VM across them is what makes "disk survives a power-cut, dies at kill" both true.

### Does it replace the Windows Sandbox path?
CireSnave, 2026-11-07, verbatim (relayed by the PM): *"Windows Hyper-V can't create a Windows guest?"* **It can.** Revision 2 of this note said the VM broker does not replace Windows Sandbox for Windows-toolchain jobs; that overstated it, and is replaced by this section. Hyper-V runs Windows guests; what differs is practical (below).
- **For Linux guests and anything that wants real SSH and real hardware-style faults (power cuts, kernel, filesystems): yes, this is the better tool,** and it has no one-VM-at-a-time ceiling (Windows Sandbox's is its own; more than one Hyper-V VM is ordinary). It would also make an A3-style pool of long-lived sessions possible for Linux work.
- **For Windows-toolchain jobs (Rust/MSVC, `slangc`, the Windows build matrix): possibly, by a Windows guest profile** (next section). Nothing is claimed either way until slice V6 measures it. The `account` launcher and Windows Sandbox stay as they are until then.
- **`wsb`'s cancellation is untouched by this design**: it does not start a Sandbox VM for a restricted account. A Hyper-V guest does not go through the Sandbox package at all, so the cancellation would not apply to it (unmeasured: item 2 above, as a non-interactive account).

### A Windows guest profile (design, every cost stated as measured or unmeasured)
Same engine, same verbs, a second `profile`: a **fixed prepared Windows base image** (VHDX, generation 2, read-only parent), a differencing disk per job, the same pool switch with no route off the host, the same connection-owned lifetime and caps. The request still names only an image and a profile.

| Question | Answer today |
|---|---|
| **Where the image comes from, and its licence** | Not decided; **CireSnave's call**, and nothing in this repo supplies one. Two kinds: a Microsoft **evaluation** image (free, time-limited, so it needs rebuilding on a schedule and its terms restrict production use) or a **licensed** Windows image (a licence per guest as Microsoft's terms define it; the host is Windows 11 Pro, and a Pro host licence is not assumed to cover guests). These licence points are from memory of Microsoft's published terms, **not checked here**; check them before a choice. Unmeasured: whether any image already exists on this box (none is known; `C:\ProgramData\Microsoft\Windows\Containers\BaseImages` is the Sandbox's own, not ours). |
| **Disk size** | Unmeasured. The host has 596.1 GB free of about 3.8 TB on `C:` (measured 2026-11-07). A prepared Windows base with a toolchain is tens of GB; per-job differencing disks grow with what a job writes and are deleted at kill. The broker's free-space floor (controls table) applies. |
| **Boot time per job, against Sandbox** | **Unmeasured for both.** This repo has no clean Sandbox boot time: an early `run=61 s` was the harness's own 60 s limit expiring and is marked ignorable in MEASUREMENTS, and the later `windows_sandbox` suite took about 303 s for 5 tests (2026-11-07), which is not a per-job figure. V6 therefore measures both a Sandbox job and a Hyper-V Windows guest on the same box, same day. A checkpointed start state is an option only if measured to help, and it conflicts with "no checkpoints" above. |
| **How the runner gets in** | Two candidates. **OpenSSH Server in the image**, key from the seed (Windows has no cloud-init: the key and address come from a first-boot script or `unattend.xml` on the seed disk, which is more fragile than Linux's), reached over the pool switch like Linux. Or **the broker's own channel**: PowerShell Direct (`Invoke-Command -VMName`) runs over the VM bus with no network, but it **needs host admin**, so only the broker could use it and it would have to proxy every command (a larger broker). Neither is tested; SSH keeps the runner and the broker separate and is the default choice. |
| **Updates and activation** | Unmeasured. A read-only base is never updated in place: a new base is built (updates applied, toolchain installed, sysprepped or not) and the broker's recorded hash is bumped, by CireSnave's setup. Activation depends on the licence choice above (an evaluation image counts down; a licensed one needs a key or KMS/AVMA, none assumed here). A differencing disk that boots an un-sysprepped parent shares its identity (name, SID) across concurrent VMs; whether that matters for jobs is unmeasured. |
| **What Windows Sandbox still does better** | It **boots a copy of the host**, so there is no image to obtain, license, build or update, and its toolchain is the host's. It is lightweight and ships with Windows. Its costs: one at a time (`CO_E_APPSINGLEUSE`), and cancelled for a restricted account. If the VM path's boot and upkeep cost more than that ceiling hurts, Sandbox wins for Windows jobs. |

No claim either way until V6 has numbers.

## The Windows Sandbox path (revision 1, kept short, "P0 to P2")
Still relevant only if a restricted account needs a **Windows** throwaway VM. A broker for it needs the same pipe, validator and cleanup, plus one thing the Hyper-V broker does not: `wsb` is started by a package whose service answered `Cancelled` to a non-interactive account.
- **P0** no broker; only the interactive/launcher user starts Windows Sandbox (today).
- **P1** the same `antspawn-broker` account, if the 321 experiment shows `wsb start` works there non-interactively (needs its own package registration).
- **P2** a dedicated low-privilege account with a login session (never `cires`); autologon stores a secret and is CireSnave's.
The board-321 experiment on `gh-runner` (register the package, run the probe) tells: `wsb list` works as `gh-runner` means the group is enough for `list` (not for `start`; run `start` and `exec -r System` too); cancelled means the session is the cause. It does not show a fresh low-privilege account behaves the same. Do not grant the group to `antspawn-run`.

## Track 2: a Windows Sandbox broker in CireSnave's own session (SB0 to SB3)
CireSnave, 2026-11-07, verbatim (relayed by the PM): *"If it can't be used by the runners, are you still thinking about setting up a broker on my account for Windows Sandbox work?  If so, that isn't really "Keep Windows Sandbox untouched"...it's creating a broker for Windows Sandbox and seeing if it can work that way.  If that's what you meant, yes.  Go ahead and start the work toward getting the broker(s) for VMs and Windows Sandbox up and running.  We can compare Windows Sandbox versus a VM with a Windows guest once we get it built."*
A small broker process runs in **his interactive session** (his account) and starts Windows Sandbox when a restricted runner asks over a pipe. Same pipe, validator and cleanup as the VM broker; it replaces "P1/P2" below with something to try first.
**Risk, stated and not blocking:** a broker in his session holds his account's rights, so its **request validation is the whole security boundary** (fixed template, no host paths, caps, per-connection lifetime). The dedicated interactive account (P2) stays as the alternative to measure after SB0.
| # | Slice | Proof |
|---|---|---|
| SB0 | **Measure:** a pipe between two accounts (broker as the user, client as `antspawn-run`); whether a Sandbox starts from the broker's session on a pipe client's request; boot to ready time; one-at-a-time behaviour (`CO_E_APPSINGLEUSE`) when two clients ask | recorded with date and ref; no elevation needed for the pipe and Sandbox parts |
| SB1 | Request validation, the same refusals as the VM broker (fixed template, no host path, caps, connection-owned lifetime), pure, runs on every CI leg | a validator that accepts a host path must FAIL its test |
| SB2 | The real run under a restricted runner account | runner-only, UNVERIFIED until it runs; recorded with date and ref |
| SB3 | **Hardening:** the same tests with the broker under a new, dedicated, minimally privileged account (no admin; an interactive session only if Sandbox needs one, which SB0 says), folder ACLs for staging only | a works/fails/degrades table for CireSnave; the account is made by his elevated paste |
**Comparison** (PM brings it to CireSnave): the same job on Windows Sandbox via this broker and on a Windows-guest VM via V6: boot time, disk, failure modes.

## What changes in the existing code and slices
- `windows_sandbox.rs`, `account.rs`: unchanged by the VM broker. A `vm` launcher (or a `vm` client module) is **new**, with its own id and matrix rows; nothing is claimed by reusing existing rows.
- **S2** (`session_proto`): the wire for the broker, with `vm.*` struct variants (unit variants ignore extra fields, measured in S2). **S3**: its DACL builder is split out; the broker needs a **listen loop**, S3's `accept` is one client. **S4**: `peer_alive` is the connection-owns-VMs watcher; the orphan sweep idea is the name-prefix cleanup. All reused, no slice changed.

## Failure modes
| Failure | What happens | Control |
|---|---|---|
| Broker down | client gets "pipe not found" and reports NotProvisioned-style, loudly; **never starts Hyper-V itself, never falls back** | test: stop the broker, a request fails with that message |
| Broker crashes with VMs up | VMs keep running (a VM is not in the broker's Job Object; unmeasured) | startup sweep by `antspawn-` prefix |
| Runner dies mid-test | pipe closes | watcher destroys that connection's VMs |
| Leak (disk or switch left) | the pool shrinks until something is free again | sweep also releases switches whose VM is gone; `status` shows pool use |
| Flood | caps and rate limit | the "overload the system" case CireSnave named: refused at the cap, tested with a client that asks for 1000 |
| Squatted pipe name | a client talks to the wrong server | `FIRST_PIPE_INSTANCE`, boot-time start, client checks the server's SID after connect (unmeasured) |
| Broker compromised | it holds Hyper-V Administrators | it runs nothing from a request; the account is dedicated and non-interactive; this is the one place the group lives |

## Slices if this is chosen (estimates, not measurements; one PR each)
| # | Slice | Proof |
|---|---|---|
| V0 | **Measure first** (PM/CireSnave's elevated steps): create/start/stop/delete a VM and an Internal switch as a non-interactive account in the group; a pipe between two accounts; a guest probing the host's ports; guest reachable from a restricted account's token; what `New-VMSwitch`'s forwarding default is | decides whether `antspawn-broker` is viable and which controls stand; recorded with date and ref |
| V1 | Request types and pure validation (every refusal above), runs on every CI leg | a validator that accepts a path, an unknown image or a huge `pubkey` must FAIL its test |
| V2 | Broker with a **fake** Hyper-V driver: listen loop, DACL, SID check, connection-owns-VMs, caps, flood, orphan sweep | current-user tests; second SID refused; disconnect destroys |
| V3 | Real driver (PowerShell/WMI), seed ISO, pool switches, base hash check | runner-only, UNVERIFIED until V0 |
| V4 | A client module and the PersistAnt power-cut as the first user | the test runs as a non-member runner account |
| V5 | Setup script additions | **CireSnave's, elevated; antspawn never creates accounts or elevates**; dry-run here |
| V6 | **Windows guest profile, after the Linux path (V4), measured** (needs a CireSnave-chosen image: evaluation or licensed): build one prepared base, boot it through the broker, record boot time to a ready SSH port against a Sandbox job on the same box, disk size of base and of a job's differencing disk, the first-boot key/address path, two guests at once, and a run as a non-interactive non-admin account | decides whether the VM path replaces Windows Sandbox for Windows jobs; recorded with date and ref; if it does not beat Sandbox, Sandbox and the `account` launcher stay |
| V7 | **Hardening: the broker under a new, dedicated, minimally privileged account** (CireSnave 2026-11-07: *"It's better to be safe than sorry.  Once we've built the broker(s), creating a new, more limited account for them to run under is a good security idea.  We'll just have to check if that hampers other aspects of the VMs it can create because of limits on that account."*). Re-run V0 and V4's tests with the broker as that account: Hyper-V rights only through the one group, no admin, no interactive session, folder ACLs for images and staging only, switch creation, PowerShell Direct (Windows guest), guest reachability. The account is created by CireSnave's elevated paste, never by antspawn | a table: each capability **works / fails / degrades**, with date and ref, so CireSnave can choose |
Then: remove `gh-runner` from Hyper-V Administrators (elevated, CireSnave's), and re-probe the `account` rows.

## Recommendation
1. Do **V0 first**; it is cheap and it settles unmeasured items 1, 2, 4 and the firewall defaults before anything is built. The earlier board-321 `wsb` experiment is still useful but no longer on the critical path.
2. Build the **general VM broker (V1 to V4)** with a dedicated `antspawn-broker` account, the connection-owned lifetime, fixed images and profiles, a pre-created switch pool with no route off the host, and the caps above. It answers CireSnave's question directly: runners ask, another account does it, and they run inside.
3. Keep Windows Sandbox and the `account` launcher as they are until **V6** measures a Windows guest profile; do not claim either way before. The Sandbox broker (P0 to P2) is re-opened only if a restricted account needs Windows Sandbox itself.
4. Questions for CireSnave (via the PM), after V0: (a) may one dedicated account hold Hyper-V Administrators; (b) the caps (VM count, memory, CPUs, disk, lifetime); (c) which Linux image(s) are the fixed set. And, for V6: (d) where the Windows base image comes from (evaluation or licensed) and who pays for the licence.
