# Measurements

Each entry: what was measured, over what, on which ref, on which date. Nothing here certifies a
launcher; only a `yes` row in `matrix/launchers.tsv` does.

## 2026-10-22: runner mechanics without the restricted account (ref: PR2b branch, 0.0.4)

`spawn-runner serve` run as the invoking user against a throwaway root (`tests/runner.rs`, Windows,
6 tests, all passed): the Job Object kills a forked survivor before `exit.code` is published; the
work dir is wiped; a file left by one job is absent for the next; `time_s = 1` stops a 20 s sleep;
copy-in/copy-out round trips; a spec above the `runner.toml` ceiling is refused before anything is
spooled; a missing setup is `NotProvisioned`.

Not measured: anything that depends on the account being a different principal (read-outside,
ACLs, firewall, the task trigger). `tests/account.rs` prints `UNVERIFIED` until
`scripts/account-setup.ps1 -Apply` has been run.

## 2026-10-22: `naive` baseline for the timing harness (ref: PR7 branch on a3cbe55, 0.0.7)

`cargo run --example measure -- naive 5`, debug build, this box: median ms provision=0 run=24 collect=0
destroy=0; 5 jobs of a 2 s sleep at once finished in 2039 ms (ok=5), so `naive` runs jobs in parallel with
about 40 ms overhead. This is the control for the real launchers: it has no isolation to pay for. `account`
and `windows-sandbox` printed UNVERIFIED (no `runner.toml`; no `WindowsSandbox.exe`); their numbers wait on the
elevated setup.

## 2026-10-22: `account` launcher live (ref: pr9-account-setup-fix on fd5b2cb, 0.0.9)

Setup was run elevated by CireSnave with `C:\Program Files\antspawn`. First task start failed with
LastTaskResult 0x80070005 (task event 203): the script's deny ACE (W,D,DC) on BinDir includes SYNCHRONIZE, which
CreateProcess needs, and `SeBatchLogonRight` had not been granted. After granting the right and removing the deny
the task ran as `antspawn-run`, LastTaskResult 0 (PM read-back). PR9 fixes the script; the fixed script itself has
NOT been re-run elevated (UNVERIFIED).

`cargo test --test account` (clean worktree of origin/main + PR9, debug): 3 passed, 0 failed, 0 ignored, 45 s:
`account_cannot_read_a_host_file_outside_its_environment`, `account_leaves_nothing_for_the_next_run`,
`account_kills_a_forked_survivor`. Known-bad control: `tests/negative.rs` runs the same probes against `naive`,
which must fail every one (passes in CI; not re-run this session).

`cargo run --example measure -- account` (5 rounds, debug): median ms provision=4 run=461 collect=0 destroy=11.
5 jobs of a 2 s sleep at once: ok=5, wall 11284 ms, so jobs run SERIALLY (about 5 x 2 s plus about 0.25 s each);
the task is registered `MultipleInstances IgnoreNew` and `serve` handles one job at a time. `concurrent` is not
claimed for `account`.

Not measured: `schtasks /Run` by a non-admin vs a named pipe; the matrix was not regenerated.

## 2026-10-22: slangc proof and `windows-sandbox` (ref: pr10-sandbox-proof on 6faf909, 0.0.10)

`cargo test --test slangc_proof -- --nocapture` (debug): `slangc_through_account_matches_the_host` PASSED twice, SPIR-V
(168 bytes) byte-identical to the host's, total 11.9 s (cold) and 2.4 s.

`windows-sandbox`: **no job has completed.** `WindowsSandbox.exe` is now present, but the logon command never ran:
`tests/slangc_proof.rs` timed out at 120 s, and again at 240 s with absolute guest paths; a hand-written `.wsb` whose
only logon command was `cmd.exe /c echo hi > C:\antspawn\out\x.txt` produced no file in 200 s; a launcher spec running
`whoami` timed out. The Application log shows APPCRASH of `WindowsSandboxRemoteSession.exe` (0.8.107.0, KERNELBASE.dll
10.0.26100.9711) around these launches (not matched one-to-one with each). Cause unknown; one suspicion is that the
remote-session client needs an interactive desktop, which a tool shell lacks (untested). Not claimed: that the feature
fails when started interactively.

Correction to the PR7 harness: `measure -- windows-sandbox` printed `run=61 s` and `ok=...`; 61 s is the harness's own
`time_s = 60` limit expiring, not a boot time, and the harness does not check `Exit`. Ignore those numbers. The matrix
row stays `no`.

## 2026-10-23: `slangc_proof` from an interactive desktop (ref: origin/main 3a6432b, 0.0.10; run by CireSnave, pasted output relayed by the PM)

`account` PASSED: provision_s=0.11 total_s=16.29 spirv_bytes=168, matches the host.

`windows-sandbox` FAILED at `tests/slangc_proof.rs:75`, which is `env.destroy().unwrap()`, with `Io(Os code 32 "being used
by another process")`: a sharing violation removing the job's temp root, not an APPCRASH. `run` and `collect` returned
`Ok` before it, so from an interactive desktop the VM boots and runs the logon command (the exit code was never
compared, since `through` unwrapped `destroy` first, so SPIR-V equality is still UNVERIFIED). The tool-shell failures
above therefore point at the non-interactive session, not at the feature. Cause of the sharing violation: the VM
holds its mapped folders open for a while after its client is killed (inferred, not observed); the fix retries
the removal (`remove_dir_retrying`, unit-tested with a held file). Not yet re-run on the desktop.

## 2026-10-23: GitHub client against the live App (ref: pr13-github-client on e0d79ba, 0.0.13)

`cargo run --example github_proof -- probe <adapter.toml>` as `cires` (the account that holds the DPAPI key), App
`antspawn-runner` (id 5441608), org installation 174476958 on ThinkersJournal: DPAPI decrypt, RS256 JWT and installation
token exchange succeeded, and `GET /orgs/ThinkersJournal/actions/runners` answered with 0 runners; the first call,
including key load, JWT and token exchange, took 760 ms (debug build, one sample). Before the PM created the group,
`runner_group` answered `Rejected: runner group ... does not exist` (the same call is the known-bad control for the run below).

Then, with the PM's group `antspawn-account` (id 3, visibility `selected`, only `ThinkersJournal/antspawn-proof`, id
1431038343, public repos off): `probe` read it back as `visibility=selected repos=["ThinkersJournal/antspawn-proof"]`, and
`github_proof -- jit <adapter.toml> antspawn-account` (through `Minter`, so `verify_group` ran first) printed:

    PROOF minted runner `antspawn-proof-1792759671` in 1763 ms (jit config 4096 bytes, not shown)
    PROOF registered: id=123 status=offline busy=false
    PROOF removed: runner `antspawn-proof-1792759671` absent from the list = true

exit 0. A `probe` afterwards listed 0 runners in the org (the same client reads before and after, so this is not an
independent check). The mint-to-listed and delete calls are verified once, debug build, one sample; no job ran, so
runner start-up and an unused runner's cleanup by GitHub's own expiry are not measured. UNVERIFIED: rate-limit
handling against GitHub, paging past 100 groups/repos/runners, and the ubuntu build of `ring`/`ureq` (CI will say).

## 2026-10-23: `windows-sandbox` STATUS_DLL_NOT_FOUND and `account` timing variance (ref: pr14-sandbox-crt on e0d79ba, 0.0.14)

CireSnave's desktop re-run on 0.0.12 (relayed by the PM): the retry fix worked (VM ran, collected, destroyed), but
`slangc` exited `Code(-1073741515)` = 0xC0000135 STATUS_DLL_NOT_FOUND. `dumpbin /dependents` (VS 18, 14.52.36815) on
`slangc.exe` and each of the seven `slang*`/`glslang*` DLLs the test stages (VulkanSDK 1.4.350.0) lists, besides OS DLLs
(kernel32, advapi32, user32, shell32, ole32 and the `api-ms-win-crt-*` Universal CRT, which Windows ships), exactly
`msvcp140.dll`, `vcruntime140.dll`, `vcruntime140_1.dll`. The test copied none of them. Red-first: the new test
`staged_tools_carry_the_visual_cpp_runtime_slangc_imports` failed with all three "not staged"; after `stage_tools` copies
them from `System32` (the installed Visual C++ Redistributable; test-time only, not committed) it passes, and the `account`
proof still passes (spirv_bytes=168). Whether the sandbox now loads them is UNVERIFIED: it cannot run from a tool shell and
needs CireSnave's next desktop run. Other missing dependencies would show as the same exit code; this fixes the three found.

`account` timing: CireSnave saw total_s 16.29, then 108.72 on the next run. Here, same code, on an otherwise ordinary
dev box (CPU 20-40% from other work), eight runs gave total_s 11.88 (cold), 2.40, 5.59, 2.16, 9.59, then five steady at
2.21-2.64 (per-phase after this PR: provision 0.05, run 2.1-2.5, collect 0.00, destroy 0.07-0.09). So the cost sits in
the run phase (Task Scheduler start plus `slangc`), varies by about 4x here with load, and 108 s was NOT reproduced; the cause
is unknown (load, Defender scanning of the 74 MB tools copy, or scheduler start latency are candidates, none tested). The
`MEASURE` line now splits the phases, so the next slow run says which phase it was.

## 2026-10-23: pinned runner fetch and unpack (ref: pr16-runner-fetch on 0e1cb0f + PR15, 0.0.16)

`cargo run --release --example runner_fetch -- <cache> <dir>` as `cires`, empty cache: download plus pinned-digest check
took 2842 ms, a second `verify_file` passed, unpack took 6995 ms (one sample each, release build, home network), and
`run.cmd`, `config.cmd`, `bin/Runner.Listener.exe` were present. The known-bad controls are unit tests (path-climbing
entry refused with no file written outside; a tampered cached file is deleted, not reused). The runner was not started.

## 2026-10-23: `account` certified (ref: pr17-certify-account on 0.0.16 + this change, 0.0.17)

`cargo test --test account -- --test-threads=1` as `cires`: 5 of 5 pass (read-outside, write-outside, clean slate, forked survivor
killed, no network), then `cargo run --example matrix` wrote `yes` rows for those five capabilities. The network probe
connects to 1.1.1.1:443; its control (this process connecting to the same address) succeeded, and the same probe run through
`naive` connected, so the probe can fail. `account` refuses nothing it was not already built to refuse; `write-outside` targets a
file under the launcher user's temp dir, which the account has no ACL on. Afterwards `spawn providers` lists `account` as
satisfying all default needs and `spawn run <cmd.exe /c dir spec> --launcher account` exited 0. One sample each; the firewall
rule (`antspawn-run: block all outbound`) is the claimed cause of the network result and was not toggled to test that.

## JobSource live (2026-10-23, main 75688eb + the `queued` mode of 0.0.22)

`cargo run --example github_proof -- queued <adapter.toml> ThinkersJournal/antspawn-proof` as `cires`: the JWT and installation-token
exchange worked (the same App listed runners in the earlier `probe`), then `GET /repos/ThinkersJournal/antspawn-proof/actions/runs`
failed `HTTP 403: Resource not accessible by integration`. Cause read from the message: the installation lacks the repository
permission `Actions: read`. Not retried; the App's permissions are CireSnave's to change. `JobSource` remains UNVERIFIED against
GitHub. Control not re-run this session: the earlier `probe` and `jit` runs (above) used the same App and token path and succeeded.

Board 264 (2026-10-23 17:57, same code at main 00b2aad): CireSnave granted `Actions: Read-only`. `queued` now answers `ok: 0
queued job(s) in 916 ms` (no 403); `github_proof perms` reads back `actions=read`, `metadata=read`,
`organization_self_hosted_runners=write` (3 granted). Zero jobs cannot tell "works" from "sees nothing", and `antspawn-proof` is
empty (contents API 404, no workflow), so there is no positive control yet: `JobSource` stays UNVERIFIED until a queued job
is listed.

Positive control (2026-10-23 18:00, same code): the PM pushed a never-running workflow `queue-probe.yml` to `antspawn-proof` (commit
417f87a2) and dispatched run 40047740331 on `[self-hosted, antspawn-nonexistent]`; `queued` then printed `ok: 1 queued job(s) in
1851 ms` and `QueuedJob { run_id: 40047740331, job_id: 120733219368, labels: ["self-hosted", "antspawn-nonexistent"] }`. Zero
before, one after, and the run id and labels are the dispatched ones, so `JobSource` is VERIFIED for the queued-run listing.
Not covered: paging past 100 runs, rate-limit handling. The PM cancels the run and removes the file after this row.

## 2026-10-24: `windows-sandbox` slangc `TimedOut` diagnosed (ref: main 00b2aad, 0.0.22; fix in 0.0.23)

CireSnave's desktop run of 0.0.22 (relayed by the PM): `account` PASSED (run_s=3.29, 168 bytes, identical to the host);
`windows-sandbox` `exit=TimedOut provision_s=0.00 run_s=241.34 destroy_s=2.02`. `provision_s=0.00` is expected, not a symptom:
`provision` only writes files, the VM starts in `run`. Cause found on this box, no VM needed: `job.cmd` ended `echo %RC%>
"...\exit.code"`, and cmd reads a one-digit number before `>` as a handle (`0>`), so the redirect never happens. Probe, cmd
batch, `RC=0`: `echo %RC%> f` left f at 0 bytes; `RC=17` left 4 bytes; `>f echo %RC%` with `RC=0` left 3 bytes. An empty
`exit.code` does not parse, so `run` polled to the 240 s limit. The 0.0.12 run returned `-1073741515` only because that code
has more digits. This fits every number in the report; it does not explain the pop-up dialog CireSnave saw, which is
UNVERIFIED on his desktop (0.0.23 adds `trace.log`/`cmd.log` and event capture to show it). Separately, from this lane's
tool shell the sandbox never starts, and now there is a reason: 0.3 s after `WindowsSandbox.exe` starts,
`WindowsSandboxRemoteSession.exe` (package 0.8.107.0) dies, Application log APPCRASH, `System.IO.FileNotFoundException: Could
not load file or assembly 'WinRT.Runtime, Version=2.2.0.0 ...'` at `InitializeGlobalVtableLookup` (event 17:50:53, test start
17:50:52.78; the same crash is logged on 10-22 and earlier on 10-23, i.e. every start from here; `WinRT.Runtime.dll` exists
in the package folder, its version unread). `guest trace.log` was never produced, so the guest never ran. Whether CireSnave's
pop-up is this crash is UNVERIFIED: the same `Get-WinEvent` on his run would show it. Tests: the generated script is run on the host;
the old form is the known-bad control (empty file for `hostname`'s code 0). Not run in a VM here. The next desktop run decides.

## Windows Sandbox crash: cause found, launcher moved to `wsb.exe` (2026-10-23 local, main bba0da5 + 0.0.29)

Replaces the earlier "host health" note, whose "cause UNKNOWN / suspect launch context" is retracted.

- Not the package, the feature or virtualization: `WinRT.Runtime.dll` (2.2.0.0) is in the package folder, Authenticode Valid,
  readable by Users, same SHA-256 in a copy; hypervisor present, `vmcompute`/`vmms`/`HvHost` running, WSL2 VMs running, no
  pending reboot, 21 GB free RAM, VBS on. The host was a console session of the same user as CireSnave's desktop.
- Cause: the client crashes only when started WITHOUT package identity. Every crash event (ours and CireSnave's 20:35) has an
  empty "Faulting package full name". Running `...\WindowsApps\<pkg>\WindowsSandboxRemoteSession.exe` directly reproduced
  `0xe0434352 FileNotFoundException WinRT.Runtime 2.2.0.0` (with `COREHOST_TRACE`, the file is in the trusted-assembly list);
  a copy of the folder run from `%TEMP%` got past that load and died with a plain `System.Exception`, the text of the pop-up
  "Windows Sandbox failed to initialize. Exception of type 'System.Exception' was thrown."; started through the package's
  app-execution alias the client stayed up and `vmmemWindowsSandbox` started, and CireSnave saw a full desktop.
- So `System32\WindowsSandbox.exe <file>.wsb`, which this launcher used through 0.0.28, starts the client without identity here.
  UNVERIFIED: why that worked on 2026-10-23 afternoon (the run that reached `destroy`). Why identity matters for the load is
  not established; the fix does not depend on it.
- Fix (0.0.29): the launcher drives the package's own CLI, `%LOCALAPPDATA%\Microsoft\WindowsApps\wsb.exe`: `start -c <config>`
  (id in about 6 s), `exec --id <id> -r System -c "cmd.exe /c C:\antspawn\in\job.cmd"`, `stop --id <id>`. A `LogonCommand` in the
  start configuration never ran (126 s, no interactive guest logon); `exec -r ExistingLogin` answers `A specified logon
  session does not exist (0x80070520)`, so the job runs as SYSTEM inside the throwaway VM (see `docs/DESIGN.md`).
- `slangc_through_windows_sandbox_matches_the_host` (run from a tool shell, no elevation, no one at the desktop): exit 0, 168
  SPIR-V bytes identical to the host's, provision 0.01 s, run 38.1 s, destroy 3.6 s, total 41.8 s; `wsb list` empty after. One sample.
- `cargo test --test windows_sandbox -- --test-threads=1` (2/2): inside the VM `C:\Projects` and the host profile are not
  readable, a read-only mount is not writable, `ping 1.1.1.1` fails; controls: `C:\Windows` readable and `%TEMP%` writable in
  the guest, and the same probe script run on the host reports `read_profile=LEAK` and `net=LEAK`. A run killed outright leaves
  its VM listed (control), and the next run's `reap_orphans` stops it. Not measured: other hosts, concurrent sandboxes, GPU.
  The matrix row for `windows-sandbox` stays `no` until the matrix probes certify it.
- A first probe of the read-only mount reported a false `LEAK`: `echo x> file >nul` lets the last redirect win and the write
  went to `nul`. The probe now uses `copy nul`; the earlier reading was a test bug, not a hole (read-only mount checked by hand: no
  file appeared on the host).

## windows-sandbox certified in the matrix (2026-10-24, 0.0.30 on main 3ab115d)

- `cargo run --example matrix` on this host re-earned 5 `yes` for `windows-sandbox` (read, write, write+exists, fork, connect), one VM per
  probe, and the 5 `account` and 5 `no` `naive` rows again; rows gained a new `probed_at` only. `--check` green with and without `--no-privileged`.
- The guest runs the Smart App Control policy (`VerifiedAndReputableDesktop`, event 3096 in its CodeIntegrity log). It lets an UNSIGNED exe
  start once per VM and then blocks it: `spawn-probe.exe sleep 0` printed rc=0 at t+1 s and "blocked by your organization's Device Guard policy"
  (rc 1073751882) on every later start, in 3 VMs; signed Windows tools (`cmd`, `curl.exe`, `powershell`) were never blocked. Consequences: (1) the
  `fork` probe could not start its own sleeper, (2) a blocked first-use exits 1, which the old probe protocol read as "no, isolated", a possible
  false pass. Fix: the sandbox probes are a `cmd` script of signed tools; `spawn-probe` now answers "no" with exit 10 and every other code is
  inconclusive; each script step checks its own tool first (exit 3). Not established: why the first start is allowed (cache or evaluation),
  and slangc staying allowed across runs (not checked whether it is signed). A job that runs its own unsigned exe twice in a VM will fail.
- Known-bad control: the same `probe.cmd` on the host answers 0 for read, write, exists and connect (and 10 for a missing file), so the
  VM's "no" answers could have been "yes". Fork probe: a signed `ping` started by PowerShell, then `destroy`, then `wsb list` empty within 30 s.

## Smart App Control switched off inside the guest (2026-10-24, 0.0.31 on main 0325da0)

- `Runner.Listener.exe --version` in a VM (runner v2.338.1 mapped read-only): first start `FileLoadException Runner.Listener.dll ... An
  Application Control policy has blocked this file (0x800711C7)`, later starts "blocked by your organization's Device Guard policy"
  (rc 1073751882); the same with `Networking` enabled. 14 files of that runner are `NotSigned` on the host (233 `Valid`).
- In the guest as SYSTEM, `reg add ...\CI\Policy /v VerifiedAndReputablePolicyState /d 0` plus `CiTool --refresh` took the state from 0x2
  to 0x0, after which `Runner.Listener.exe` started (and stopped at a read-only `C:\runner\_diag`, a mapping choice, not a block).
- A fresh guest reads 0x2 (evaluation) or 0x1 at job start. One write was lost once in four fresh VMs (the state came back as 0x1), so
  `job.cmd` repeats the write until it reads 0x0. `unsigned_exes_start_repeatedly_in_three_consecutive_sandboxes_and_host_policy_is_untouched`:
  control (raw `wsb`, no switch-off) start1=0, start2=start3=1073751882; through the launcher 9 consecutive VMs (3 runs x 3), every one of 3
  starts of an unsigned exe exit 0, `[sac-after tries=1]` in 7 and `tries=2` in 2; the host's state value and the SHA-256 of every active
  policy file identical before and after each run. Not measured: a full runner job (needs the live adapter, next PR).

## Matrix regeneration (2026-10-24, main 767b614, capant 0.0.7)

- `cargo run --example matrix` on this host, `account` set up: its probes ran (read, write, write+exists, fork, connect) and
  re-earned 5 `yes`; `naive` rows unchanged (5 `no`, each naming its probe; the rest untested). Rows gained `probed_at`
  only. `cargo test --test account -- --test-threads=1`: 5/5. `--check` green with and without `--no-privileged`.

## Pending (needs the elevated setup, CireSnave)

- `schtasks /Run` by a non-admin through the task security descriptor vs ACL'd named pipe.
- `cargo run --example measure -- windows-sandbox`: start latency, cleanup, concurrency; negative tests for it.
