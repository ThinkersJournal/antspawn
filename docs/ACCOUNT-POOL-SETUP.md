<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Account pool setup (A3 P4): CireSnave's elevated paste

The pool is N restricted local accounts so N sessions run at once, each as a different account, so one cannot read
another's files (`concurrent`, `no-read-other-session`; probes in `tests/session_probes.rs`, real run in
`tests/account_pool.rs`). Board 420 chose N = 4 (CireSnave 2026-11-12: "420: B."). The script is `docs/proof/PASTE-pool-accounts.ps1`.

## What it makes (per account `antspawn-pool<i>`, `i` = 1..N)

| Thing | Detail |
|---|---|
| Local user | random password, used once to register the task and discarded; in no group beyond `Users` |
| Directory tree | `%ProgramData%\antspawn-pool\antspawn-pool<i>\{spool,results,work,runner.toml}`: **its own** tree, ACLs as the single account's (`scripts/account-setup.ps1`) but naming only this account and the launcher |
| `SeBatchLogonRight` | this account added (the task cannot start without it) |
| Firewall | `antspawn-pool<i>: block all outbound` for this account's SID |
| Task | `AntSpawnPool<i>`: `spawn-runner.exe --root "<tree>" serve` as the account; the launcher user may run it (`GRGX`), nobody else |
| Deny | each account is denied `C:\Projects` (`-DenyPath`, its own ACE; minutes on a large tree; `-DenyPath @()` skips it) |
| `%ProgramData%\antspawn-pool` | no inheritance; admins/SYSTEM full; the launcher read and traverse; `leases\` writable by the launcher only |

**Why separate trees, not one shared spool:** with one shared `work`/`results` each account's ACE inherits into the others'
job directories, so one could read another's; and a runner scanning a shared spool could host a session leased to another
account. Per-account roots need no shared-directory ACL change at all.

## What it does not touch

No ACL on `C:\Program Files\antspawn`, on `%ProgramData%\antspawn`, or on the existing `antspawn-run` account, its task or its
firewall rule. Every directory it creates is new; every ACE it adds names the account it belongs to (plus the launcher).
It does **not** register the Windows Sandbox package or change `spawn-wfp`.

## Run it

Needs a runner that understands `--root` (P2b, merged in #113; build `5C04BC47...` of main `4a11c30`) deployed first (board 416) (`docs/REDEPLOY-RUNNER.md`); the script
refuses unless the deployed `spawn-runner.exe` has the hash you pass.

1. Dry run, not elevated (changes nothing; prints every step):
   `& C:\Projects\antspawn\docs\proof\PASTE-pool-accounts.ps1 -Count 4 -RunnerSha256 <sha256 of the deployed spawn-runner.exe>`
2. Elevated (a Windows Hello or UAC prompt is the approval): the same line plus `-Apply`.
   It prints `APPLY` per step and `PASS`/`FAIL` per property read back; the exit code is non-zero if any failed.
   Safe to re-run (each account gets a new random password and its task is re-registered).
3. Not elevated, after it passes: set `ANTSPAWN_POOL=antspawn-pool1:AntSpawnPool1,antspawn-pool2:AntSpawnPool2,antspawn-pool3:AntSpawnPool3,antspawn-pool4:AntSpawnPool4` and
   `ANTSPAWN_POOL_PROBE=<spawn-probe.exe the accounts can run>`, run `cargo test --test account_pool -- --nocapture` and record the
   result in `docs/MEASUREMENTS.md` (date, ref). Only a pass flips the matrix rows.

## Undo

`& ...\PASTE-pool-accounts.ps1 -Count 4 -RunnerSha256 <hash> -Undo -Apply` (elevated). For every `antspawn-pool*` account found
it stops and unregisters the task, removes the firewall rule, removes the account's deny ACE from each `-DenyPath`, removes
the account from `SeBatchLogonRight`, deletes the account and its profile, then deletes `%ProgramData%\antspawn-pool`; each
removal is read back (`PASS` = gone). Without `-Apply` it only prints. Nothing of the single `antspawn-run` setup is touched.

## Measured and not

Measured 2026-11-12 (ref: this branch): the script parses under Windows PowerShell 5.1 and PowerShell 7; the dry run for `-Count 2` (the size is now 4; a `-Count 4` dry run is NOT measured, only the parameter range 1-8 allows it)
prints the steps above and changes nothing; a wrong `-RunnerSha256` is refused; `-Undo` without `-Apply` changes nothing.
**Not measured: any `-Apply`.** The first real run is CireSnave's; nothing here shows the ACLs, the batch right, the tasks or the
read-backs behave as written, and the script proves setup, not isolation (only the pool probes can).
