# AntSpawn

Applies to every agent working in this repo. Portfolio rules in `C:\Projects\CLAUDE.md` apply in full and
this file only adds to them. Read `HANDOFF.md` first and begin its first unblocked task.

- **What it is:** a sandboxed job runner (`spawn`): one clean environment per job, declared needs only,
  outputs collected, environment destroyed. Spec: `C:\Projects\ANTSPAWN-SPEC.md`; design: `docs/DESIGN.md`.
- **Tested, never asserted:** a capability is off until a `matrix/` row says `yes`; every test has a known-bad
  control that must FAIL the launcher it should fail (the `naive` launcher fails them all).
- **Launchers never elevate and never create accounts.** Privileged setup is one-time, separate and
  CireSnave's; a launcher that finds it missing says so loudly and stops.
- **Privileged tests** (account, windows-sandbox) are runner-only and named UNVERIFIED in CI until a runner
  exists. Never `#[ignore]` them. Record local runs in `docs/MEASUREMENTS.md` with date and ref.
- **Secrets are names only**, never logged or written to env or files. Not implemented yet: a spec listing
  one is refused.
- **Repo rules:** pinned actions, `permissions: contents: read`, `deny.toml`, SPDX header on every source file
  (`bash .github/spdx_gate.sh`), MIT OR Apache-2.0, crates.io deps only, `CHANGELOG.md` entry and version bump
  per PR (the PM allocates the number), `cargo fmt` before pushing.
- **No imports from other lanes' trees.** Lanes cannot push here: send the PM the commit SHA.
- Windows toolchain gotchas: see `HANDOFF.md`.
