# Account-backed Dispatch: a runner job on a restricted local account (ROADMAP row 21)

Design note, docs only. Written 2026-10-30 at `origin/main` ecb6f76 (0.0.46) for the PM's [TASK] (cpufish's AOCL tests need host resources, so
the runner runs on the host as a restricted account, not in Windows Sandbox). **Nothing here was run**; facts are read from this repo at that
ref and from agentlife's `docs/RUNNER-ACCOUNTS.md` (read from branch `docs/runner-setup-8b` at e034c99, **not yet on its `main`**).

**Ruled (board 341, CireSnave 2026-10-30, verbatim: "341: A."):** this path may use a named ACL'd pipe and outbound network, departing from board 260 ruling (b) as this note describes. Where the text below says "open ruling" or "needs its own CireSnave decision", that decision is made for the pipe and the network; the elevated provisioning is still CireSnave's and still not run. How the outbound access is limited was not part of the ruling as relayed; the design below still recommends limiting it to GitHub's hosts.

## What exists, and the one thing that does not carry over

`SandboxDispatch` (0.0.38) runs one runner VM per minted JIT config behind two seams: `RunnerBackend` (start a VM, call a hook with its id) and
`JitTransport` (get the config into that VM). The `account` launcher runs a *spec* job as `antspawn-run` through a scheduled task
(`spawn-runner serve`) with a spool directory; the launcher user never holds the account's password.

**The board 260 stdin pipe does not carry over.** An anonymous pipe is inherited by a child that the sender starts. On this path the process that
must receive the config (the shim, as the restricted account) is started by Task Scheduler, not by the host, and the host cannot start a process
as that account (it has no password, by design). So the host cannot hold the write end of an anonymous pipe to it. Same story as the sandbox
(where `wsb exec` forwards no stdin), different cause. The channel that works across two accounts on one machine without a file is a
**named pipe**, which is exactly the open item already in DESIGN item 7 ("schtasks /Run versus an ACL'd named pipe").

## Design

One runner = one account = one root (agentlife's model, RUNNER-ACCOUNTS "Design" 1 and 2): `ghr-<repo>` (or the name the setup script derives),
root `C:\gh-runner-<account>` writable by that account only, **no group memberships**. `antspawn-run` (spec jobs) is a different account and
is not reused for runners.

1. **Per-account task.** The elevated setup (CireSnave) registers, per runner account, a task that runs `spawn-jit-shim --pipe <name> <runner>`
   as that account, in the same style as `account-setup.ps1` (which registers `AntSpawnRunner`). The task, the account, the root and the ACLs are
   created by the script, never by antspawn.
2. **Shim pipe mode (built in 0.0.50, tested with the current user only; the SID arrives as `--allow-sid <sid>` because the shim runs as the runner account, not the launcher; the ACE is write plus `FILE_READ_ATTRIBUTES`, since write alone denied the named SID when measured).** `spawn-jit-shim --pipe <name> --allow-sid <sid>` creates a named pipe server, **one instance only** (`nMaxInstances = 1`),
   `PIPE_REJECT_REMOTE_CLIENTS`, with a DACL that grants write to the launcher user's SID and nothing to anyone else (not the account itself, not
   `Everyone`), accepts exactly one connection, closes the pipe, reads the config (same limits and checks as stdin mode), and starts the runner
   with the value in that one child's environment, stdin null. In memory only, never a file, never inherited, never logged: the same promises as
   the listener of board 322. A second connection or one after the first finds no pipe.
3. **Host side.** `AccountBackend: RunnerBackend` triggers the account's task (`schtasks /Run`, see "Not measured"), waits for the pipe to exist
   (retry window), and a `PipeTransport: JitTransport` connects as the launcher user and writes the config once. The runner is the pinned
   v2.338.1, copied from the read-only cache into a fresh per-run directory under the account's root and removed afterwards; JIT runners are
   ephemeral, so the runner exits after one job and deregisters itself. "Outcome" and `busy()` semantics of `SandboxDispatch` are unchanged.
4. **Between runs (built in 0.0.53 as the shim's `--fresh`, tested as the current user only; the account's temp and profile folders are NOT wiped yet).** The task's last step wipes the per-run directory and the account's temp and work folders (the existing `serve` semantics:
   the result marker appears only after the process tree is dead and the directory is gone), and a kill-on-close Job Object holds the tree.
5. **Network (ruled, board 341).** The existing account is outbound-firewalled (`account-setup.ps1`: "block all outbound"). A runner needs
   outbound HTTPS to GitHub, which **contradicts board 260 ruling (b)** ("the runner gets network only inside Windows Sandbox"). That ruling was
   about the VM path; the account path needs its own CireSnave decision before any live run (draft question for the board: may a per-runner
   account have outbound access limited to GitHub's required hosts, and how is "limited" enforced when Windows Firewall rules take addresses,
   not host names). Board 341 ruled A: outbound network is allowed on this path. Recommendation kept: limit it to GitHub's hosts, not wider by default; how to enforce that is a setup-script detail for the elevated step.
6. **Residue.** Anything that can run as the launcher user can connect to the pipe first. The launcher user already holds the GitHub App key that
   mints the config, so this widens no trust boundary; the sandbox path's residue (any guest process) is larger. A second account's processes
   cannot connect (DACL).

## What a 3-run proof looks like

Three consecutive full runner jobs on the account, each: mint a JIT config, trigger the task, deliver over the pipe, runner registers, takes a
queued job of `antspawn-proof`, finishes, deregisters, the account is clean. The job (a workflow in `antspawn-proof`) does, as the account:

* **Positive:** reads AOCL (`C:\Program Files\AMD\AOCL-Windows`, which `BUILTIN\Users` can read per agentlife's measurement) and writes a
  sentinel into its work dir.
* **Negative control A (the one the PM asked for):** tries to read a sentinel file in **another account's root** (`C:\gh-runner-<other>\canary`)
  and must FAIL; the same read **as the other account** must succeed (proves the canary exists and is readable by its owner).
* **Negative control B:** tries to write into the other account's runner directory and must FAIL (agentlife finding 2: a shared root makes every
  runner account writable; this proves each root is private).
* **Naive control:** the same job run as the launcher user (no account) reads the canary, so the test can fail; the proof records that it does.
* **Clean slate:** run N+1 does not find run N's sentinel; after each run no process of the account remains and the work dir is empty; the
  runner is gone from the repo's runner list.
* **Pipe controls (no account needed, loopback-style):** a second connection is refused, a connection after the first is refused, the shim
  exits after one, a connection from another account is denied, and the config appears in no environment of the shim itself, no file and no log.

Recorded in `docs/MEASUREMENTS.md` with date, ref and the runs' ids. "Working" means all three runs pass; one pass is not the word.

## What cannot be measured without CireSnave's elevated step

Creating the runner account, its root and ACLs, the per-account task, and any firewall rule: all elevated, all CireSnave's, none run by a
lane. Without them these are **UNMEASURED**: that a non-admin `schtasks /Run` can trigger the account's task (the existing open question;
fallback is a pipe-triggered start, which needs a resident process as the account and is a different design), that the pipe DACL admits the
launcher user and nobody else on this machine, anything involving the second account (negative controls A and B), and the network path.

## What needs board 321 and what does not

Board 321 is the Windows Sandbox feature and the runner account for **sandbox** tests. It is **not** needed for this path. This path needs its
own elevated provisioning (account, task, ACLs) and, now ruled (board 341), the network above; the provisioning itself still needs its own board item (the PM raises it).

| Step | Needs |
|---|---|
| This note | nothing |
| `spawn-jit-shim --pipe`, one-shot semantics, DACL-for-current-user test, `PipeTransport`, `AccountBackend` with fakes (as 0.0.38 did) | nothing: all testable without elevation or a second account |
| Setup script for per-runner account/task/ACL (printed, dry-run by default) | nothing to write; running it is CireSnave's elevated step |
| Negative controls A and B, non-admin `schtasks /Run`, DACL against a real second principal | the elevated provisioning (new board item), not 321 |
| A live run against `antspawn-proof`, then 3 runs | provisioning (the network is ruled, board 341); for cpufish also the GitHub App installed on cpufish (an owner action) |
| Running these tests in CI on a self-hosted runner (rules 5.1g/h) | board 321's runner half |

## Slices

1. Shim `--pipe` mode + tests (loopback-style, current user only). 2. `PipeTransport` + `AccountBackend` with fakes. 3. Setup script and the
board question (PM). 4. Live run and the 3-run proof after provisioning (network ruled, board 341). Each slice one PR, each saying "not run live".
