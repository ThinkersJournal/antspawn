<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Session-scoped sandbox: sizing A1, A2, A3 (options note, no code)

Asked by the PM 2026-11-03 for the agent-loop lane's Tier S (`agent-loop` `docs/SANDBOX.md` section 5.5, asks A1-A3). **Docs only: nothing
here is built, scheduled or ruled.** Evidence: this repo at `0dea9bf` (main `cc98a2c` plus #66), `docs/DESIGN.md`, `docs/MEASUREMENTS.md`,
`matrix/launchers.tsv`, `src/runner.rs`, `src/bin/spawn-jit-shim.rs`. A "slice" is one PR of the size this repo's PRs have been (a few
hundred lines of Rust plus tests with a known-bad control, or one script plus its paste). Sizes are estimates, not measurements.

Candidates are the two launchers that isolate: `account` (a restricted local account) and `windows-sandbox` (a throwaway VM). `naive` isolates
nothing and is not a candidate. Neither launcher has a session concept today; `Launcher::provision -> Environment { run, collect, destroy }`
(DESIGN item 3) is one job.

## Summary

| Ask | `account` | `windows-sandbox` | Needs a ruling |
|---|---|---|---|
| A1 long-lived job | feasible; 4 slices | feasible in principle (`wsb exec` many times into one VM: **unmeasured**); 3 slices | no |
| A2 in-place workspace | write-back: 4 slices; bind: 1 slice plus a ruling | `rw` mapping exists for one job; reach is the whole mapped folder | **yes, CireSnave** (who may write to project trees) |
| A3 N concurrent sessions | pool of N accounts; 4 slices plus N elevated pastes | **no**: one VM at a time, measured | no, but N and the accounts are CireSnave's one-time setup |

Recommended order: A1 on `account` first (everything else sits on it), then A3 (pool), with A2 decided in parallel because it is a question,
not code. `windows-sandbox` stays Tier J (job per call) for untrusted or networked code.

## A1. Long-lived job (a session: open, many `exec`, close)

**Today.** `account`: one `exec` is one kill-on-close Job Object; `serve` wipes the work dir before `exit.code` appears (DESIGN item 7). The
task is the only start path and ignores a second start while running. `windows-sandbox`: `wsb start`, one `wsb exec`, `wsb stop` per job.

**Account, two ways to carry the calls in:**

1. *Spool per exec.* A session is a directory `work/<sid>` that is not wiped between execs; each exec is a spool entry (`spool/<sid>/<n>/spec.toml`)
   run by the existing `serve` loop inside a per-session Job Object that stays open until `close`. Least new surface (reuses `check_spec`, the ACLs and
   `schtasks /Run`), but each exec pays a task start and a poll, and the one `serve` loop is occupied by a session. Latency per exec: **unmeasured**
   (ROADMAP row 13 only measured per-job cost for `windows-sandbox`, and retracted its own figure).
2. *Session host over a pipe.* A new binary started by the task as the account holds the session and reads framed `exec` requests from a named pipe
   whose DACL grants write to the launcher SID only. This is the mechanism `spawn-jit-shim --pipe <name> --allow-sid <sid>` already uses to cross two
   accounts without a file (one instance, remote clients rejected, protected DACL), generalised from one message to many. Lower latency, and the
   natural carrier for streaming output (A5) and per-exec cancel (A4). More new code; the pipe protocol is a new attack surface and needs its own
   negative controls (a second client, a wrong SID, an oversized frame).

Recommendation: way 2 for Tier S, because way 1 cannot stream and pins the runner. This is my judgement, not a measurement.

**Slices (account, way 2):** (1) `Session` trait beside `Launcher` (`open_session`, `exec`, `close`; `close` also on `Drop`) and a `session` capability
in the matrix, off until a row says `yes`; (2) the session host binary and pipe protocol, with the shim's DACL tests as the template; (3) cleanup of an
abandoned session (owner process gone: host ends, work dir and the account's profile temp wiped); (4) probes with known-bad controls: state persists
across execs, nothing persists after `close`, a killed owner still ends the session. Plus a `spawn` CLI verb and the result contract per exec.

**Windows Sandbox:** `wsb start` once, `wsb exec` per call, `wsb stop` at close. Boot is paid once per session: `run` 5.5 to 6.9 s and `destroy`
3.0 s on 2026-10-29 (two runs), but the 2026-11-01 re-measurement found the bare VM alone at **9.2 to 23.4 s** on the loaded host, so budget
roughly 10 to 25 s to open. Folders are mapped at `wsb start` only, so inputs must be mapped up front. The host-to-guest channel cannot be the
TCP one the JIT shim uses (the VM has network off; the 260 ruling allows network only for the runner case); a mapped-folder spool is the
network-off option. Slices: session trait (shared with account), VM lifecycle plus a mapped-folder exec channel, orphan-VM reaping (exists for one
job via `%TEMP%\antspawn-wsb-<id>.owner`; extend it).

**Isolation cost.** State now persists between execs inside a session by design, so `clean-slate-next-run` does not describe a session; the new
capabilities `clean-slate-next-session` and `no-read-other-session` (ask A6) each need a probe and a known-bad control before they read `yes`.
For `account` a session can also write the account's real profile by absolute path, and that survives into the next session on the same account:
`scripts/runner-account-setup.ps1` already records this limit for the runner; a session needs a profile wipe at close, which does not exist.

## A2. In-place workspace (write-back or bind)

**Today.** `account` copies `ro` sources in and declared outputs out; `rw` mounts are refused; `C:\Projects` carries a deny ACE for the account
(`scripts/account-setup.ps1`, propagated to every file under it). `windows-sandbox` maps declared `rw` folders for one job
(`src/launchers/windows_sandbox.rs` maps `ro` and `rw` mounts); I found no test in this repo that certifies `rw` write-through.

**Options:**

- **B1. Bind with an ACL grant** (account). Give the account Modify on the project tree. The edits are in place with no copy. Windows evaluates an
  explicit allow before an inherited deny, so an explicit grant on the subtree should beat the inherited `C:\Projects` deny (documented ordering,
  **not measured here**). The cost is reach: the account, and so anything the agent runs, can then write every file in the tree, including `.git`,
  hooks, CI workflows, build scripts and `CLAUDE.md`. 1 slice (grant and revoke at session open and close, with a negative control that a
  sibling project stays denied) plus the ruling below.
- **B2. Session working copy plus reviewed write-back** (account). Copy the tree in at open, record a manifest of hashes; at `sync` or `close` the
  launcher user (not the account) diffs the working copy against the manifest and applies a change set to the real tree under a policy. 4 slices:
  (1) manifest and diff, (2) apply with conflict detection when the real tree changed meanwhile, (3) the policy filter (path allowlist and denylist,
  size cap, refuse reparse points and symlinks, which could redirect a write outside the tree), (4) controls: a denied path is refused, a conflict
  is reported not overwritten, a junction is refused. Copy-in cost grows with tree size (`copy_tree` is a plain recursive copy; **not measured**
  on a real project).
- **B3. `rw` mapped folder** (windows-sandbox). The VM writes the host folder directly through the mapping. Same reach as B1 for the folder
  mapped, with no review step, and the writer is SYSTEM inside the VM. Needs a probe before it can be offered.

**Isolation cost, all three.** Any write path into a real tree is a path to host code execution if the host later runs what was written (a test run,
a build script, a git hook, a CI workflow) outside the sandbox. B2 is the only option where a reviewer or a policy sits between the write and the
tree. This is the reason it is a ruling and not an engineering choice.

**Questions for CireSnave (not decided here):**
1. May a sandboxed session write to a project tree at all, or only to a working copy that a person or a policy applies?
2. If it may write in place, which paths are never writable (`.git/`, hooks, `.github/`, `.claude/`, `CLAUDE.md`, scripts the host runs)?
3. Is a deletion by a session allowed to reach the real tree, or does write-back refuse deletes?
4. May file-only tools run host-side under path rules (agent-loop open question 3)? That is a separate downgrade, and it changes what B1/B2 must cover.

## A3. N concurrent sessions

**Today.** `account`: one task, one runner, one job at a time; `concurrent` reads `no`, "no probe for this capability yet" (`matrix/launchers.tsv`).
`windows-sandbox`: a second `wsb start` fails `0x800401F6 CO_E_APPSINGLEUSE`; measured 2026-10-29 on this host (two runs, wall 9.8 s, about
serial). **Windows Sandbox cannot serve N sessions** on this build; no other VM launcher was read.

**Account: a pool of N accounts.** The repo already runs two runner accounts, each with its own root, pipe and task (`ghr-spawn1`, `ghr-spawn2`;
`runner-account-setup.ps1 -Name`). Isolation between sessions is then the OS boundary between two SIDs plus per-root ACLs, the property the proof's
negative controls A and B are written to show; **those controls have not yet run live**, so "no session reads another's workspace" is unmeasured.
Slices: (1) generalise `AccountLauncher` and `Layout` from the fixed `antspawn-run` and `AntSpawnRunner` to a named account set; (2) a lease
allocator (a session takes a free account, released on close, reclaimed when its owner dies); (3) a `concurrent` probe: two sessions at once, wall
time not serial, each denied the other's canary, with a known-bad control that fails if both sessions share an account; (4) fold the firewall block
rule into the pool's setup (`account-setup.ps1` blocks the account's outbound traffic; `runner-account-setup.ps1` deliberately does not, per board
341, so a pool built from the runner script would have network on). Plus **one elevated paste per account** from CireSnave (launchers never create
accounts), and N is fixed at setup time.

**Isolation cost.** Sessions share the kernel, the machine and its resources: A7's per-session RAM cap exists only as a per-job `ram_mb` today, and
`cpu` is refused, so one session can starve the others. An account reused by a later session is not clean unless the profile wipe from A1 exists.

## What I did not verify
`wsb exec` repeated into one VM; any session latency for `account`; the ACL ordering claim in B1; `rw` write-through in the VM; copy-in time on a real
project; the live negative controls A and B; whether a pipe session host survives the Job Object and kill semantics the shim uses today.
