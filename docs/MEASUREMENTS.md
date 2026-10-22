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

## Pending (needs the elevated setup, CireSnave)

- `schtasks /Run` by a non-admin through the task security descriptor vs ACL'd named pipe.
- The three negative controls against the real account (`cargo test --test account`).
- Windows Sandbox: feature not enabled on the dev box (`WindowsSandbox.exe` absent).
