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

## 2026-10-23: pinned runner fetch and unpack (ref: pr16-runner-fetch on ab0f9d1, 0.0.16)

`cargo run --release --example runner_fetch -- <cache> <dir>` as `cires`, empty cache: download plus pinned-digest check
took 2842 ms, a second `verify_file` passed, unpack took 6995 ms (one sample each, release build, home network), and
`run.cmd`, `config.cmd`, `bin/Runner.Listener.exe` were present. The known-bad controls are unit tests (path-climbing
entry refused with no file written outside; a tampered cached file is deleted, not reused). The runner was not started.

## Pending (needs the elevated setup, CireSnave)

- `schtasks /Run` by a non-admin through the task security descriptor vs ACL'd named pipe.
- `cargo run --example measure -- windows-sandbox`: start latency, cleanup, concurrency; negative tests for it.
- Windows Sandbox: re-run `slangc_proof` from an interactive desktop on the retry fix.
