# Changelog

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
