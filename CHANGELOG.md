# Changelog

## 0.0.8

- `antspawn::adapter::github` and `docs/ADAPTER-GITHUB.md`: design and types for the GitHub App adapter (per-job
  `generate-jitconfig`, ephemeral runners). Redacted `Secret`, `KeyProvider` (a key path inside the job sandbox is
  refused), strict `AdapterConfig` with runner groups, group verification, `Minter` with backoff and no retry storm.
  No network or signing code yet; measurements stay UNVERIFIED.

## 0.0.7

- `examples/measure.rs`: times any launcher (provision, run, collect, destroy medians over N rounds; N jobs at once
  against a 2 s sleep for concurrency). `account` and `windows-sandbox` print UNVERIFIED while not set up.

## 0.0.6

- `tests/slangc_proof.rs`: PR4-proof harness. Runs `slangc` (Unpopped's shader compiler) through `account` and
  `windows-sandbox` and requires SPIR-V byte-identical to a direct host run; prints `MEASURE` timings.
  Runner-only: prints UNVERIFIED while a launcher is not set up. Not yet run against a real launcher.

## 0.0.5

- `examples/matrix.rs`: regenerates `matrix/launchers.tsv` by running the negative probes (`--check` for CI).
  `naive` rows come from real probe runs (all `no`, probe named); `account` and `windows-sandbox` are
  `no` / `UNVERIFIED: not set up` and are never `yes` without a privileged run.
- Probes moved to `antspawn::probes` (shared by the tests and the generator); new write-outside probe.

## 0.0.4

- `account` launcher + `spawn-runner` service binary: the job runs as a restricted account whose password
  nobody holds; spool/results directories, copy-in/copy-out, Job Object kill-on-close (the only `unsafe`,
  Win32 calls in `spawn-runner`), runner-side ceilings in `runner.toml`, loud `NotProvisioned`.
- `scripts/account-setup.ps1`: one-time elevated setup, dry-run default, `-Apply`, reads state back.
  Dry run tested; `-Apply` NOT yet run (needs CireSnave, elevated).
- `tests/runner.rs` (runner mechanics as the current user, passing) and `tests/account.rs` (real account;
  prints UNVERIFIED until the setup exists). `docs/MEASUREMENTS.md` started.
- New dependency: `windows-sys` (Windows only).

## 0.0.3

- `windows-sandbox` launcher: generated `.wsb` (no network, no vGPU, no clipboard; declared mounts only),
  `job.cmd` run at logon, `exit.code` polled, VM closed and host folders removed on destroy and on drop.
  cmd.exe metacharacters in the command, env or paths are refused, never escaped. Absent feature gives a loud
  `NotProvisioned`; the launcher never enables it. UNVERIFIED: no matrix row, no privileged run yet
  (the Sandbox feature is not enabled on the dev box).
- Fix: the two CLI tests shared one spec file and raced.

## 0.0.2

- `naive` launcher: a plain child process, the known-bad control. Never certified; `spawn run`
  refuses it for any spec that needs isolation.
- `spawn run <spec> [--launcher id] [--results dir]`, `spawn providers <spec>`, `spawn matrix`.
- Negative tests (read outside, clean slate, forked survivor) with the `spawn-probe` helper;
  `naive` must fail all three. Empty `matrix/launchers.tsv` (no launcher certified yet).
- Not in this PR: the `account` launcher (blocked on how it authenticates; see HANDOFF).

## 0.0.1

- Repository scaffold: CI (jobs build-test, msrv, deny, spdx),
  `deny.toml`, `CODEOWNERS`, `SECURITY.md`.
- Job spec types (strict TOML; `secrets` refused), capability set, matrix parser (PersistAnt's TSV
  format), launcher trait and provider selection (`providers`, `open_named`).
- `spawn check <spec>`.
