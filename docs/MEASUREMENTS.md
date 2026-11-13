# Measurements

Each entry: what was measured, over what, on which ref, on which date. Nothing here certifies a
launcher; only a `yes` row in `matrix/launchers.tsv` does.

> **Superseded statements (corrected 2026-10-30):** the lines below that say the Windows Sandbox "never starts" or "cannot run" from a tool shell (the 2026-10-22 entry's
> "which a tool shell lacks", the 2026-10-23 entry's "cannot run from a tool shell", and the 2026-10-24 slangc entry's "from this lane's tool shell the sandbox never starts") describe the
> `WindowsSandbox.exe <file>.wsb` stub. Since 0.0.29 the launcher drives `wsb.exe`, and the `windows-sandbox` tests run from a tool shell without elevation (see "Windows
> Sandbox crash: cause found" below, the 2/2 run). The older entries stay as history.

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

## Smart App Control switched off inside the guest (2026-10-24, 0.0.32 on main 0325da0)

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

## Stale control: the raw guest no longer blocks unsigned exes (2026-10-29, main 033823a, wsb package 0.8.107.0)

- **Symptom.** `cargo test --test windows_sandbox -- --test-threads=1` fails its raw-`wsb` control (`start1=start2=start3=0`, expected 0 then 1073751882) on
  unmodified main 033823a and on PR #38; the other 4 tests pass. Last recorded green run of it: 2026-10-24 (layer below). Matrix probes do not use this control.
- **Measured today (10:38-10:45 PM local, raw `wsb`, no launcher code, no switch-off):** 7 fresh VMs read `VerifiedAndReputablePolicyState = 0x2` at start;
  an unsigned exe started 24/24 times in one VM over 2 minutes (every 5 s, state stayed `0x2`) and 3/3 in each of 3 more VMs. The guest's active SAC policy is
  `{1283ac0f...} VerifiedAndReputableDesktopEvaluation` (event 3096), not the enforcing `VerifiedAndReputableDesktop` named on 2026-10-24. Guest OS 10.0.26100.9818.
  Guest `CI\Policy` carries `SAC_PreviousState=0xffffffff` and `OOBESafetyTimer` = 2026-10-30T03:10:31Z (76 s after the host's boot).
- **What changed on the host since 2026-10-24** (read, not guessed): the Sandbox layer `1ef1ee21` (created 10-22 09:33) was superseded by layer `adff5482`
  (created 10-28 16:57, last written 10-29 20:09:37); a new snapshot `583c85a8` was created 10-29 20:12:34; KB5132402 installed 10-28; Endpoint Security policy
  `{60fd87f8}` rewritten 10-28 16:09; two reboots 10-29 20:07 and 20:09; KB5127936 (2026-10 preview) installed 10-29 20:12. The package is the same 0.8.107.0.
  Host SAC state is `0` with `SAC_PreviousState=1`, key last written **2026-07-30 16:07**, so the host's own setting did not change and is not the cause.
- **Working hypothesis, not established:** the guest image was rebuilt on 10-28/10-29 and its SAC evaluation restarted, so the guest audits instead of enforcing.
  Nothing here separates the 10-28 layer from the 10-29 update, and no way was found to boot the old layer for a before/after.
- **Layer diff attempted, read-only, not possible cheaply (PM: only if cheap).** The new layer `adff5482` is readable; its 8 active CI policy files are the same names as the host's
  (SHA-256 prefixes recorded by name only; `{60fd87f8}` dated 10-28 16:09). The old layer `1ef1ee21`'s files are not readable ("The file cannot be accessed by the
  system" on every `.cip`) and its 5 `*_BASE` hives are 0 bytes, so no content diff was made. Left open.
- **Positive control failed to bite.** Forcing `VerifiedAndReputablePolicyState=1` plus `CiTool --refresh` inside one fresh guest read back `0x1` but the
  exe still started 3/3, so this probe has not yet been shown able to see a block in today's image. One VM; not conclusive either way.
- **Consequence.** Documents that said the guest blocks unsigned exes are annotated (DESIGN) and withdrawn (CHANGELOG 0.0.39) until re-proven. The 3-consecutive-VM
  pass in "Smart App Control switched off inside the guest" is vacuous today (it would pass without the switch-off). Not measured: whether the runner's unsigned DLLs
  load without the switch-off in this image; whether enforcement returns later (evaluation periods end).

## Matrix regeneration (2026-10-24, main 767b614, capant 0.0.7)

- `cargo run --example matrix` on this host, `account` set up: its probes ran (read, write, write+exists, fork, connect) and
  re-earned 5 `yes`; `naive` rows unchanged (5 `no`, each naming its probe; the rest untested). Rows gained `probed_at`
  only. `cargo test --test account -- --test-threads=1`: 5/5. `--check` green with and without `--no-privileged`.

## `wsb exec` forwards no stdin (2026-10-29, main 630257b, wsb from MicrosoftWindows.WindowsSandbox)

- `wsb exec --help` lists --id, -c, -d, -r, --raw: no stdin option. Measured: `printf SECRETPROBE123 | wsb exec --id <id> -r System -c
  'cmd.exe /c findstr "^" > C:\antspawn\out\stdin.txt'` in a fresh VM (Networking Disable, `out` mapped rw) ran (`Process exited with
  code: 1`, findstr's code for no input) and left `stdin.txt` at 0 bytes. The piped bytes never reached the guest process.
- Consequence: board 260's anonymous-pipe hand-off cannot cross `wsb exec`; the shim inside the guest needs another channel that is not a
  file, argument or variable (proposal to the PM, not built). One VM, one run: a single measurement, no control that stdin works elsewhere
  (host `findstr` reads piped stdin by definition).

## Windows Sandbox: latency and concurrency (2026-10-29, main 55221cc, 0.0.36)

- `cargo run --example measure -- windows-sandbox 2` on this host, debug build, probe plus VC++ runtime DLLs in a mapped folder (the example
  could not run this launcher before: it mapped a file at a relative guest path; fixed). Sequential, 2 rounds, no-op job: median
  provision 2 to 4 ms, **run 5.5 to 6.9 s** (VM start to `exit.code`), collect 4 to 5 ms, **destroy 3.0 s**. Two runs, not a distribution.
- **Concurrency: not available.** Two jobs started at once: one ran, the other failed at `wsb start` with `0x800401F6
  CO_E_APPSINGLEUSE` ("Application cannot be run more than once"); wall 9.8 s (about serial). So Windows Sandbox runs one VM at a time on
  this build, and the launcher must never claim `concurrent`. Control: the same example runs 2 sequential rounds fine. `wsb list` empty after.

## Toolchain loading cost per job (2026-10-30, main f08a257 + examples/toolchain.rs, 0.0.42)

- `cargo run --example toolchain 3` on this host, debug build, Windows Sandbox, sequential, variants round-robined, median of 3. Toolchain =
  `slangc.exe` + `slang*`/`glslang*` DLLs + VC++ runtime = **54.2 MiB** (VulkanSDK 1.4.350.0). Job compiles a trivial compute shader to SPIR-V.
- Run phase (VM start to `exit.code`): `none` (hostname.exe, the VM alone) **4354 ms**; `mapped` (slangc straight from the read-only mapped folder)
  **5780 ms**; `copied` (xcopy the folder to the guest disk, then run from there) **5483 ms**. Total with provision, collect and destroy: 6842 / 8297 / 7287 ms.
- **SUPERSEDED by the 2026-10-31 entry below** (it reported medians of 3 with no spread; with the spread shown, the 1.1 to 1.4 s difference is inside the
  host's noise). Originally: the toolchain costs about 1.1 to 1.4 s per job over the bare VM, and copying it in first is not slower than running from the
  mapping. There is no "golden image" to compare: Windows Sandbox boots a fresh image every
  job, so the only choices are a mapped cache or a copy. Not measured: a larger toolchain (the 54 MiB set is small), a cold host file cache, `account`.
- Control: the `none` variant uses the same launcher, folders mapped and VM lifecycle; a launcher or mount problem would fail it, and it ran.

## Toolchain loading cost per job, with spread and a larger set (2026-10-31, main d446332 + examples/toolchain.rs `PAD_MIB`, 0.0.66)

`cargo run --release --example toolchain 5` on this host, Windows Sandbox, sequential, variants round-robined, **median (min to max) in ms, 5 rounds**.
`PAD_MIB=512` adds a 512 MiB synthetic filler file the job never reads to the toolchain folder (total 566.2 MiB), so a mapped folder pays nothing for it and a
copy pays for every byte; a real toolchain's unused share will differ. The host was **loaded**: CPU 60% before the second run and 90% after, 1.2 GB free memory, several `claude` processes and `bun` running;
the load before the first run was not recorded.

| Toolchain | Variant | Run phase | Total (provision to destroy) |
|---|---|---|---|
| 54.2 MiB | none | 12598 (9164 to 15143) | 15330 (13453 to 18570) |
| 54.2 MiB | mapped | 14820 (7524 to 18493) | 18939 (10152 to 23324) |
| 54.2 MiB | copied | 14538 (9391 to 16127) | 18112 (12806 to 20700) |
| 566.2 MiB | none | 9461 (8426 to 23371) | 12557 (11295 to 27014) |
| 566.2 MiB | mapped | 8435 (8341 to 12680) | 11251 (10879 to 14917) |
| 566.2 MiB | copied | 9875 (8435 to 25836) | 12693 (11490 to 29822) |

- **The variants are not distinguishable on this host.** The bare VM alone ranges 9.2 to 15.1 s (54 MiB set) and 8.4 to 23.4 s (566 MiB set), against 4.4 s in the
  quiet 2026-10-30 run, and `mapped` has a lower minimum than `none` in the first set, which no real cost can do. The earlier "1.1 to 1.4 s per job" is within that noise and is
  not a result. Whatever the toolchain costs, at 54 MiB it is smaller than the host's own variation, and even 512 MiB of unread filler copied into the guest did not
  separate `copied` from `mapped` at 5 rounds (medians 9875 vs 8435, minimums 8435 vs 8341).
- **Golden image: not measurable here, and not available in Windows Sandbox.** The sandbox boots a pristine image every launch (nothing installed survives), so the
  alternatives are the mapped folder and the copy. For the `account` launcher an installed toolchain would need a directory the account may read, which is an
  elevated setup step and CireSnave's; nothing was changed. **Unmeasured:** `account` at all; a cold host file cache; a real large toolchain; any measurement on a quiet host
  (needs the other lanes idle, and `copied` and `mapped` may separate there).
- Recommendation from this evidence only: nothing here separates `mapped` from `copied` (the 54 MiB medians go one way, the 566 MiB medians the other), so keep `mapped` because it needs no copy step, and re-measure on a quiet host before deciding anything finer.

## Pending (needs the elevated setup, CireSnave)

- `schtasks /Run` by a non-admin through the task security descriptor vs ACL'd named pipe.

## 2026-10-31: pipe controls on the real runner account (ref: main 238b9b7, shim built from 0e10cbe hash 3D83E4CB; standard user cires)

After CireSnave's `-Apply` of `runner-account-setup.ps1` (account `ghr-spawn1`, task `AntSpawnRunner-spawn1`, pipe `antspawn-spawn1`):
cires listing `C:\gh-runner-spawn1` was **denied** (the launcher has no access to the account's root). The live task descriptor read through
`Get-TaskSdProblems`: 0 problems. `schtasks /Run` as cires: `\.\pipe\antspawn-spawn1` appeared within the 20 s poll (probe method not recorded; see the later entry); a first connection reported as delivering a dummy
base64 config (44 bytes, not a real JIT config; client direction not recorded); a second connection was refused; afterwards no shim process remained, the pipe was gone and the task was Ready with
LastTaskResult 0. **Not measured:** the shim's owner (`GetOwner` came back empty for it, once, as cires; why is not known), what the shim did with the
dummy config (no log), and whether the runner started (no new `Runner.*` process was seen in one sample after the shim had gone; the box's other runners were excluded by pid).
GitHub contact was not observed, but the only check was that no shim process remained to hold a connection. One run, one sample.

## 2026-10-31: spawn1/spawn2 pipe probes, and LastTaskResult 2 (ref: main ca72587; standard user cires; one box)

The spawn1 entry above does not record how the pipe was probed or which client direction was used; treat its 0 as the result of an unknown probe.
spawn2 (`ghr-spawn2`, pipe `antspawn-spawn2`): cires denied on `C:\gh-runner-spawn2` and `\canary`; task descriptor 0 problems (control with the SIDs swapped: 2).
Two probes, same dummy 16-byte base64 text, each run after a `schtasks /Run`, task Ready beforehand:
- **A** = `Test-Path \.\pipe\<name>` to wait for the pipe, then a two-way client that reads: connect timed out; LastTaskResult **2**. Seen 2 of 2 on spawn1 and 3 of 3 on spawn2.
- **B** = list `\.\pipe\` with `Directory.GetFiles` (does not open the pipe), then a write-only client sends the text: wrote 16 bytes; LastTaskResult **0**. Seen 1 of 1 on each.
Exit 2 in the shim is `refuse()` (empty, oversized or not-base64 config, `src/bin/spawn-jit-shim.rs`). Consistent with, but not proven by, `Test-Path` itself opening the one allowed connection and
closing it empty; the shim's stderr was not captured and the pipe's access mask was not read, so the cause is **unmeasured**. Either way the shim took exactly one connection and left nothing behind (no shim, no pipe, task Ready).
Not measured: the shim's reaction to a real config, whether the runner started, any GitHub contact.

## 2026-10-31: failed account dispatch, how it ends (ref: branch off main d446332; standard user cires; `ghr-spawn1`; one sample per case)

`cargo run --example dispatch_failure -- AntSpawnRunner-spawn1 antspawn-spawn1`, through the real `SandboxDispatch` + `AccountBackend` + `SchtasksTask` + `PipeTransport`, dummy config never delivered:

| Case | Outcome | `nothing_ran` | Took | Left behind (immediately and +5 s) |
|---|---|---|---|---|
| task name does not exist | `Err("`schtasks /Run /TN AntSpawnRunner-no-such-task` failed (exit code: 1): ERROR: The system cannot find the file specified...")` | true | 0.2 s | task absent; no pipe; 0 shim processes |
| real task started, transport told a pipe the shim does not create (3 s window) | `Err("could not open pipe antspawn-no-such-pipe: entity not found")` | true | 4.1 s | task Ready; pipe `antspawn-spawn1` not listed; 0 shim processes |

So both leave nothing running, which is what the retry rule needs. **Not measured:** that the shim was actually running during the second case (only that none remained, so `end()` stopped it or it had exited); a pipe that is busy or refuses the user; a failure AFTER delivery (the runner starting and dying), which the code treats as final and which no run here produced; what `wait` reports when the shim refuses a config (the earlier probe B ended with `LastTaskResult` 0, and `wait` only reads the task status, so it would report `Ok`).

## 2026-11-06: W0, does an explicit grant beat the inherited `C:\Projects` deny (ref: main `cdcd1c8`; `spawn.exe` built 2026-10-31 from main of that date; standard user cires; `antspawn-run`; one sample per case)

Throwaway tree `C:\Projects\antspawn-w0-scratch\{tree\src,tree\.git,other}` (inherits the account's `(OI)(CI)(N)` deny from `C:\Projects`; removed afterwards). Probe `cmd /c` run AS the account through `spawn run --launcher account`; each operation names an absolute host path. "Delete" is judged from the host's view after, because the account's own `if exist` is false for a file it cannot see.

| Operation | Baseline (no grant) | Explicit `(OI)(CI)(M)` on `tree` | plus explicit deny on `tree\.git`: icacls `(W,D,DC)` | plus explicit deny on `tree\.git`: exact rights (no Synchronize) |
|---|---|---|---|---|
| `src` create / read / append | denied / denied / denied | **ok / ok / ok** | ok / ok / ok | ok / ok / ok |
| `src` delete | not tested | **ok** (file gone on the host) | ok | ok |
| `.git` create / append / delete | denied | (not run separately) | denied / denied / denied | denied / denied / denied (file still present on the host) |
| `.git` read | denied | (not run separately) | **denied** | **ok** |
| sibling `other` read / write | denied / denied | denied / denied | denied / denied | denied / denied |

- **An explicit grant on the subtree beats the inherited deny** (ordering confirmed for this tree, one sample). The deny still bites a sibling, so a bind grant reaches only the granted tree.
- **An explicit deny on a subpath beats the grant**, so protected paths (`.git`, `.github`, `.claude`, `CLAUDE.md`, hooks) can be kept read-only inside a bound tree.
- **Use exact rights, not icacls `W`:** `W` includes SYNCHRONIZE, so `(W,D,DC)` also made `.git` unreadable (a `git status` inside the session would fail). The working deny was `WriteData, AppendData, WriteExtendedAttributes, WriteAttributes, Delete, DeleteSubdirectoriesAndFiles` set through `Set-Acl`; `icacls /deny` merges into an existing deny ACE, so re-running with narrower rights silently kept the wide one (one invalid run discarded for that reason).
- **Not measured:** rename or create-directory inside the denied path, `.git` replaced by a file or junction, files created by the account (their owner and inherited ACL as seen by the launcher user), grant cost on a large tree, and a grant surviving a crashed launcher (the journal in `docs/SESSION-PLAN.md` W4).

## 2026-11-06: session host over a named pipe (S3), as the current user

Ref: branch `session-s3` (stacked on `session-s2`), `tests/session_host.rs`, run on this box as the interactive user in a temporary layout (no restricted account). One sample each.

| Check | Result |
|---|---|
| Pipe DACL naming my SID | connects; `exec` echo/type shows state persisting across execs; `close` answered `closed`, host exits, `session.end` = `closed` |
| Pipe DACL naming LocalService (S-1-5-19) | my open of the pipe is `PermissionDenied` (control for the line above: same code path, other SID) |
| Second client while one is connected | `ERROR_PIPE_BUSY` (231) |
| Client drops without `close` | host exits within the bound, `session.end` = `disconnected` |
| Detached survivor (`start /b hb.cmd`, appends a line a second) | grows while the session is up (control), stops growing after the host's job ends |

- **Not measured:** the host AS the restricted account (needs a `spawn-runner` redeploy; `tests/account_session.rs` prints UNVERIFIED until then), a remote client (`PIPE_REJECT_REMOTE_CLIENTS` is set, never exercised), a killed owner during a long exec (the host reads the disconnect only between frames), exec latency, the 60 s accept window.
- The runner-only test took 60 s here: the deployed `spawn-runner.exe` predates `session`, the task ran `serve`, which ignored the spool entry, so `open_session` waited out its grace and the test reported UNVERIFIED.

## 2026-11-07: Hyper-V state on this box, read-only (main `3df0a14`; as `cires`, non-elevated token holding `Hyper-V Administrators`; one sample; nothing created or changed)

- Windows 11 Pro build 26300, 63.2 GB RAM, 32 logical CPUs. Services `vmms`, `vmcompute`, `hns` Running. `Get-VM` works non-elevated and lists **no VMs**.
- `Get-VMSwitch`: `Default Switch` and `WSL (Hyper-V firewall)`, both `Internal`. `Get-NetIPInterface` (IPv4): `Forwarding` Disabled on both vEthernet interfaces. `Get-NetNat`: empty.
- `Get-NetFirewallHyperVVMCreator`: one creator, WSL (`{40E0AC32-46A5-438A-A0B2-2B479E8F2E90}`); `Get-NetFirewallHyperVVMSetting`: default inbound Block, outbound Allow. So the per-VM Hyper-V firewall is shown here for WSL only.
- OpenSSH client present (`C:\Program Files\OpenSSH\ssh.exe`). Hyper-V Enhanced Session Mode enabled.
- **Not measured:** creating or starting a VM or switch (by anyone), a non-interactive account doing so, a pipe between two accounts, the forwarding default of a new switch, guest reachability from a restricted account, the per-VM firewall for a non-WSL VM. Used by `docs/SANDBOX-BROKER.md`.

## 2026-11-06: can `wsb.exe` start Windows Sandbox as a restricted account? (PM board 321 task; ref: main `9658e68`; `spawn.exe` built 2026-10-31; one sample per case)

Method: `spawn run --launcher account` runs `cmd /c probe.cmd` AS `antspawn-run` (non-interactive: started by the `AntSpawnRunner` scheduled task; `query session` answers "No session exists", `%SESSIONNAME%` empty).

| As | Command | Result |
|---|---|---|
| `antspawn-run` | `where wsb` | `C:\Users\antspawn-run\AppData\Local\Microsoft\WindowsApps\wsb.exe` (the package registration from 2026-11-02/03 is in effect) |
| `antspawn-run` | `wsb list` | `Status(StatusCode="Cancelled" ... System.OperationCanceledException: The operation was canceled.)`, exit -2146233088 |
| `antspawn-run` | `wsb start -c "<Configuration><vGPU>Disable</vGPU><Networking>Disable</Networking></Configuration>"` | the same `Cancelled`, exit -2146233088; no VM was created (a `WindowsSandbox*` process listing afterwards shows only the interactive session's `WindowsSandboxServer.exe`) |
| `cires` (interactive; control) | `wsb list` | succeeds (prints an empty list) |

- **So the answer to "can it start at all as the account" is: not today.** Registering the package makes `wsb.exe` resolve, but every call that reaches the Sandbox service is cancelled. Not tested: `WindowsSandbox.exe <file>.wsb` as the account (the 2026-10-29 entry already records that stub starting without package identity and its client crashing as the interactive user).
- **Candidate cause, untested: membership of `BUILTIN\Hyper-V Administrators`.** `net localgroup "Hyper-V Administrators"` lists exactly `cires` and `gh-runner`; `antspawn-run` is not in it (the other runner accounts were not read here). That is correlation on one machine, not a cause. The other candidate is the absence of an interactive session. These two are not told apart by anything measured so far.
- **Cheapest discriminating experiment (needs the PM's elevated step, not run):** `gh-runner` is already in the group and has no session either. Register the Sandbox package for `gh-runner` (the same step done for `antspawn-run`) and run the probe as it: if `wsb list` works there, group membership is the cause and no interactive session is needed; if it is also cancelled, the session (or something else) is. Adding `antspawn-run` to the group would also test it, but see the cost.
- **Cost of the group fix:** Hyper-V Administrators means complete control of Hyper-V on this machine. Granting it to the account that runs untrusted jobs widens that account's reach beyond "restricted"; it would need CireSnave's ruling and the `account` isolation rows re-probed.
- **Not measured:** why the service cancels (its log or ETW was not read), a start as `ghr-spawn1`/`ghr-spawn2` (the HANDOFF says all three behave alike), and anything about the group fix itself.

## 2026-11-07: row 9 re-run, the stale SAC control bites again (ref: session-s4 `5368c04`; run as cires, interactive; host Windows 10.0.26300 with KB5127936 installed 2026-10-30; one run)

`cargo test --test windows_sandbox` with `RUST_TEST_THREADS=1` (about 303 s): **5 passed, 0 failed**, including `unsigned_exes_start_repeatedly_in_three_consecutive_sandboxes_and_host_policy_is_untouched`, whose raw-`wsb` control asserts the exe starts once and is then blocked (`start1=0`, `start2=1073751882`). On 2026-10-29 the same test failed because the guest only audited. So the guest enforces again, after the host update of 10-30 (and whatever else changed). Not measured: the package version and guest build for this run, the guest's policy state (`VerifiedAndReputablePolicyState`) and event 3096 vs the enforcing policy, which update changed it, or whether it stays that way. One sample; the 2026-10-29 section above stands as history, and the claim "the guest blocks unsigned exes" stays withdrawn until a few more runs agree (row 9 stays open for repeats).

## 2026-11-07: SB0, a Windows Sandbox started from the broker's interactive session on a pipe client's request (ref: `scripts/sb0-broker.ps1` on branch broker-windows-guest; wsb 0.8.107.0; host Windows 11 build 26300; one box, cires's interactive session, no elevation anywhere)

Setup: `sb0-broker.ps1` runs as `cires` (PowerShell 7, hidden window) and serves one pipe `antspawn-sb0` whose protected DACL names `cires` (full) and `antspawn-run` (`0x12019f`). The client is a PowerShell script run AS `antspawn-run` through `spawn run --launcher account` (`spawn.exe` built from main `9658e68`'s tree, no runner redeploy needed). On the line `start` the broker runs `wsb start --raw` with no configuration (no mapped folders, no network settings), then polls `wsb exec --id <id> -c "cmd.exe /c echo ready" -r System` until it succeeds, replies with the timings, and runs `wsb stop` when the client disconnects.

| Fact | Result |
|---|---|
| **A pipe between two different accounts** (unmeasured item 1 for the Sandbox track) | Works. The server saw the client identity `TUF-LAPTOP\antspawn-run` after `RunAsClient`. |
| **Control: a DACL that does not name the client** | The same client, with the DACL naming `gh-runner` instead, got `Access to the path is denied` (1 sample). |
| **A Sandbox starts from the broker's session on a restricted account's request** | **Yes**, 4 of 4 runs, while a direct `wsb start` as `antspawn-run` is Cancelled (#73). The Sandbox belongs to the broker's session, not the client's. |
| **`wsb start` returns after (broker side)** | 16.9 s, 11.3 s, 11.3 s, 10.9 s |
| **Ready (`exec` works inside) after the request** | 20.1 s (the first request; why it was slower is not established, a cold start is one possibility), 12.1 s, 13.2 s, 12.6 s. Ready came 1 to 3 s after `wsb start` returned. This is the whole boot as measured here, not Sandbox's best case; the host was otherwise lightly loaded and not controlled. |
| **Lifetime follows the client** | In all 4 runs `wsb list` was empty and the broker logged `wsb stop` within about 4 s of the client exiting; after the last run `wsb list` was empty. |
| **One at a time** | **Not measured here.** The broker serves one pipe instance, so a second client waits on the pipe; whether a second `wsb start` during a first is refused was measured earlier (`CO_E_APPSINGLEUSE`). |

NOT measured: a job's files getting in and results out (the Sandbox here has nothing mapped; the existing launcher maps folders), running anything as the client inside the Sandbox, network, the broker surviving the user logging out or locking the screen, more than one request at a time, a bad request (this prototype has no validation, by design), CPU/RAM cost, and what happens if the broker's session is not interactive. The broker is a harness: it holds `cires`'s rights, so its (unbuilt) validation would be the whole boundary.

Also from the V0 script work: PowerShell 7 has no `NamedPipeServerStream` constructor taking a `PipeSecurity`; use `NamedPipeServerStreamAcl.Create` (the first attempt died silently and the client saw a plain connect timeout), and `RunAsClient` takes a `PipeStreamImpersonationWorker`, not an `Action`.

## 2026-11-08: `account_session` on the new `spawn-runner.exe` (ref: main `ad6cc3a`; run as `cires`, interactive; one run)

Deployed first (CireSnave's elevated paste, board 399; read back by the PM and by me): `C:\Program Files\antspawn\spawn-runner.exe` sha256 `3A5102B3...4E3`, 1,346,048 bytes = the CI artifact of main `3333a683`. Before: `14610444...F46` (748,032 bytes).

`cargo test --test account_session` (`RUST_TEST_THREADS=1`): **1 passed, 0 failed** in 3.83 s (30 s wall with the build). The test did not print `UNVERIFIED`, so both guards passed: the `AntSpawnRunner` task is registered and `open_session` succeeded.

What the one test asserts, all held: a session opened through Task Scheduler runs `whoami` as a user other than the launcher (`antspawn-run`, not `cires`); a file written by one `exec` is read by the next (state persists between execs); `close()` returns Ok; a second session starts without the file (nothing survives close).

What the old path did (from the code and the earlier HANDOFF, not re-timed here because the old exe is gone): the old `spawn-runner.exe` has no `session` verb, so the pipe never appeared, `open_session` waited out its wait and returned an error, and the test printed `UNVERIFIED: the deployed spawn-runner cannot host a session yet`. The run order was not repeated on the old exe today.

Not measured: the real second user as launcher (the owner check is test-verified only), latency of `open_session` and of an `exec` on the account (the whole test was 3.83 s, two sessions and five execs), many sessions at once, what the account can reach from inside a session, logout or lock behaviour. One run on one day.

## 2026-11-12: both workspace modes and `account_session` on the real account, with the `--root` runner (ref: main `4397631`, test file `tests/account_workspace.rs` of this branch; run as `cires`, interactive; one run each, `RUST_TEST_THREADS=1`)

Deployed first (CireSnave, board 416; read back by the PM and by me): `C:\Program Files\antspawn\spawn-runner.exe` sha256 `5C04BC47...EA1` = the CI build of main `4a11c30` (carries `--root`). Before: `3A5102B3...4E3`.

- `cargo test --test account_session`: **2 passed, 0 failed**, no `UNVERIFIED` line.
- `cargo test --test account_workspace -- --nocapture`: **2 passed, 0 failed**. Throwaway trees under `%TEMP%`, a real `git init` in the bind tree.
  - **copy-back, as `antspawn-run`:** the project went in; the account edited `a.txt` and added `n.txt`; before `apply` the real tree was unchanged (control); `stage_changes` listed exactly `a.txt` modified and `n.txt` added; a wrong `set_hash` wrote nothing; `apply_staged` with the right hash landed both; the next `stage_changes` was empty (the base moved).
  - **bind, as `antspawn-run`:** the grant (denies first, then Modify) opened a session in the real folder; the account's edit reached the real file in place; a sibling folder that was NOT granted could not be read (control); `close` left no grant journal entry.
  - **`git status` inside a bind session: FAILS.** Exit 128, `fatal: failed to stat '...\AppData\Local\Temp': Permission denied`, with a real repository (not an empty `.git`), and the same with `GIT_CEILING_DIRECTORIES` set to the project's parent. Printed, not asserted. Cause not measured (git stats the project's parent, which the account cannot read here). Whether it fails the same under `C:\Projects\<name>` is not measured either. So a model that runs `git status` in a bind session will see this error today.

Not measured: the copy-mode `changes`/`apply` as JSON lines through `spawn session` (only through the library on the real account); bind with the leased pool accounts; grant cost on a large tree; a crash leaving the grant; rename or mkdir inside a denied path; many sessions at once. One run on one day. No matrix row is changed by this: the rows for the two workspace modes still need a capability id and a probe with a known-bad control.

## 2026-11-13: matrix rows `workspace.copy-back` and `workspace.bind` for `account` (ref: main `3c9461b` plus this branch; run as `cires`, interactive; one `cargo run --example matrix`)

Probes `Probes::violates_workspace_copy_back` and `violates_workspace_bind`, through the real restricted account (`antspawn-run`, runner 5C04BC47). Result: **both `yes`** for `account`; `naive` is `no` for both (the known-bad control, also asserted in `tests/workspace_probes.rs`); `windows-sandbox` has no probe (`no`). The same run re-probed every other row and none changed value (only `probed_at`).

Not measured: bind with leased pool accounts; `git status` inside a bind session (still fails, cause unmeasured); the JSON-line `changes`/`apply` through `spawn session` on the real account.
