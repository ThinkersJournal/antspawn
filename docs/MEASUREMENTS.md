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

## Pending (needs the elevated setup, CireSnave)

- `schtasks /Run` by a non-admin through the task security descriptor vs ACL'd named pipe.
- `cargo run --example measure -- windows-sandbox`: start latency, cleanup, concurrency; negative tests for it.
- Windows Sandbox: feature not enabled on the dev box (`WindowsSandbox.exe` absent).
