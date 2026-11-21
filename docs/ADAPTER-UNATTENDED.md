<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Starting the adapter unattended (design proposal, ROADMAP row 36)

Written 2026-11-21 at `work-3`. **RULED by the PM the same day: option A**, behaviour 1 to 6 as written, no sweep mail, the status file only (below). Built: `scripts/adapter-run.ps1`, `scripts/adapter-task.ps1`, `src/adapter/status.rs`, `spawn adapter --status-file`. **The task is NOT registered**: that makes the adapter poll GitHub at the next logon, and live running is held (first live job only on `antspawn-proof`).

**The gap:** `spawn adapter` is a foreground process (`src/main.rs` `adapter`, `command::run`). Nothing starts it after a reboot or a logon, restarts it when it dies, or tells anyone it is down.
Exit codes (read, `docs/ADAPTER-GITHUB.md`): 0 clean, 2 could not start (config or key), 3 `--once` ended badly.

## What constrains the answer

* **Whose account.** The GitHub App key is stored with DPAPI for the current user (`--check` loads it that way, `%LOCALAPPDATA%\antspawn\github-app`, read 2026-11-21), and CireSnave ruled the broker runs as `cires` (board 483, "C"). So the adapter must run **as `cires`**: another account, or SYSTEM, cannot decrypt the key. The runner accounts (`antspawn-run`, `ghr-spawn1/2`) must never run it: they are the sandboxed side.
* **No elevation by a lane.** Registering anything machine-wide is CireSnave's one-time setup.
* **Preconditions.** It is useless without the egress proxy listener, the pool switches and the VM base (rows 38-40); a start without them must fail loudly, not loop.

## Options

| | A. Scheduled task "at logon of `cires`", run only while logged on | B. Scheduled task "at startup", stored password | C. Windows service |
|---|---|---|---|
| Runs after a reboot with nobody logged in | no (starts at `cires`'s next logon) | yes | yes |
| Can decrypt the DPAPI key | yes (interactive session) | **unmeasured**: a stored-password batch logon loads the profile, a service-style or S4U logon may not | no, unless it runs as `cires` with a stored password, then same question as B |
| Stored credential on disk | none | the account password in the task store | the account password in the SCM |
| Needs a wrapper | no (a task with restart settings) | no | yes: a service shim (new code, new attack surface) |
| Registration | a user can register their own at-logon task (read: standard Task Scheduler; **not measured here**) | elevated, CireSnave's | elevated, CireSnave's |

**Recommendation: A**, because it keeps the key where it is, stores no password and needs no new code; the laptop is CireSnave's daily machine and is logged on while it works. B is the upgrade if unattended-after-reboot matters, **and only after** a measurement that DPAPI decrypts under that logon (a one-minute test with `spawn adapter --check` as the task). C is not recommended: a service shim is new code that holds the same key.

## Behaviour to propose for A

1. **Entry point:** a script `scripts/adapter-run.ps1` (to be written after the ruling) that runs `spawn adapter --check` first and starts `spawn adapter` only on exit 0; on a check failure it writes one line to a log the runner accounts cannot read and **exits without retrying** (a config or key error does not fix itself).
2. **Restart:** Task Scheduler settings: restart on failure, 1 minute apart, 3 times; then stop and stay stopped until the next logon or a manual start. Exit 3 and a crash restart; exit 2 does not.
3. **One instance:** the task's multiple-instances policy "do not start a new instance"; the adapter also holds a named mutex so a manual start cannot double it (small code, with the script).
4. **Visibility:** the log line plus a status file the PM's sweep can read (time of last tick, groups, jobs in flight). Today the PM would otherwise learn the adapter is down only from a queued job that never starts.
5. **Stop:** `Stop-ScheduledTask`; the adapter finishes the in-flight job's teardown (the broker's lifetime cap still ends a VM if the adapter dies mid-job).
6. **Undo:** `Unregister-ScheduledTask`; the script prints the exact line.

## Not decided here

How the PM wants the down-notice delivered (a PM message from the sweep, a file); whether B's DPAPI measurement is worth CireSnave's time; whether the status file belongs in the broker's results folder. Each is a question for the PM, not CireSnave, until it needs a privileged step.

## Built (PM ruling 2026-11-21: A; "a status file only ... the PM sweep reads its age and reports a stale tick")

* `spawn adapter --config C --status-file S` writes `S` (atomically) after every tick: `tick_unix`, `pid`, `groups`, `jobs_in_flight` (0 or 1: the dispatcher runs one job at a time), and holds `S.lock` open with sharing denied, so a second start fails at once and a dead process frees it (a Windows test covers both). No secret is in the file.
* `scripts/adapter-run.ps1`: runs `--check`; on failure it logs, records the exit code in `status.json` and exits 0 so Task Scheduler does **not** restart a config or key error; otherwise it runs the adapter and, when it ends, adds `last_exit_code` and `exited_unix` to `status.json` (no BOM) and exits with the adapter's code (0 and 2 stay stopped; a crash or 3 restarts). Default status folder `%LOCALAPPDATA%ntspawndapter` (private to the user). **The broker's results folder named in the ruling does not exist on this box** (`%ProgramData%ntspawn-broker` absent, read 2026-11-21): pass `-StatusDir` to point the file there once it does.
* `scripts/adapter-task.ps1`: registers task `AntSpawnAdapter` for the current user only (at logon, interactive, limited, no stored password, restart 3 x 1 minute, no time limit, one instance), dry run by default, `-Apply` reads each property back, `-Undo -Apply` removes it. Measured 2026-11-21: parses clean under `powershell.exe`; the dry run printed the plan and changed nothing; the wrapper, run against a missing config with a real `spawn.exe`, logged the check failure, wrote `last_exit_code` 2 into an existing status file without losing its other fields, and exited 0. **Not measured:** `-Apply` (it would start live polling), that registration needs no elevation, a restart after a crash.
