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

## Pending (needs the elevated setup, CireSnave)

- `schtasks /Run` by a non-admin through the task security descriptor vs ACL'd named pipe.
- The three negative controls against the real account (`cargo test --test account`).
- `cargo run --example measure -- account` and `-- windows-sandbox`: start latency, cleanup, concurrency.
- Windows Sandbox: feature not enabled on the dev box (`WindowsSandbox.exe` absent).
