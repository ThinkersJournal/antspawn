# Changelog

## 0.0.30

- `windows-sandbox` matrix probes (`probes::sandbox`), run as `cmd` scripts of signed tools: the guest runs Smart App Control, which blocks an
  unsigned exe after its first start (measured). Each probe checks its own tool first (exit 3 = inconclusive); the same script on the host is the
  known-bad control. `spawn-probe` answers "no" with exit 10 (was 1) so a blocked run cannot read as isolated; `account` follows.

## 0.0.29

- `windows-sandbox` now drives the package's `wsb.exe` (`start`, `exec -r System`, `stop --id`) instead of
  `WindowsSandbox.exe <file>.wsb`, whose identity-less client crashed on `WinRT.Runtime` (board 253, measured). The proof
  through it passes from a tool shell; no `LogonCommand`, no `taskkill` by image name.
- `run` fails within about 2 s, naming the Application-log event, when a `WindowsSandbox*.exe` crash follows the request.
- A run killed outright leaves its VM; the next run stops it (`antspawn-wsb-<id>.owner` in the temp dir, own ids only).
- New `tests/windows_sandbox.rs`: guest cannot read `C:\Projects` or the host profile, a read-only mount is not writable,
  network off, orphan reaped; the same probe on the host is the known-bad control. UNVERIFIED where the sandbox is absent.
- CI: the advisory `portfolio` job repinned to coderipper 30e012b7 (PM notice). `docs/MEASUREMENTS.md`, `docs/DESIGN.md`.

## 0.0.28

- Docs: `HANDOFF.md` rewritten to current state. No code change.

## 0.0.27

- Board 260 (CireSnave, "A: 1 B: 1"), host end of the runner hand-off: `spawn-jit-shim <runner> [args...]` reads the JIT config
  from stdin (an anonymous pipe), refuses anything that is not plausible base64 (empty, over 64 KiB, not text), and starts the
  runner with `ACTIONS_RUNNER_INPUT_JITCONFIG` set on that one child only; nothing prints the value. `adapter::handoff::start_shim`
  starts the shim and writes the `Secret` to its stdin. Tests (`tests/jit_shim.rs`) include controls: a wrong value and an unset
  variable fail the same check. NOT built: running the shim inside Windows Sandbox (the only place the runner gets network),
  which waits on the sandbox starting; the watcher still has no real `Dispatch`.

## 0.0.26

- First real regeneration of `matrix/launchers.tsv` with CapAnt 0.0.7: the file gains a `probed_at` column (`check` ignores
  it). No `environment` column (ruling A, 2026-10-24: one committed file must match CI on every OS). `matrix --check` still
  drops `inherited_from` from the generated side, and now prints the carried-over rows it hides (generated side) and any
  committed rows marked inherited (`Report::carried`), so review sees which rows rest on trust.
- `account` probes re-run on this host (5/5 `yes` rows re-earned), recorded in `docs/MEASUREMENTS.md`.

## 0.0.25

- Docs: `CLAUDE.md` records the ruled JIT-config exception to "secrets are names only" (CireSnave, board 260, "A: 1 B: 1"):
  in-memory pipe to a shim for the runner's JIT config, runner network only inside Windows Sandbox. No code change.

## 0.0.24

- The matrix is CapAnt's: `matrix.rs` wraps `capant::parse` (still refusing an unknown capability or a duplicate row), `Registry`
  ranks and re-checks through `capant::evaluate`, and `examples/matrix.rs` is a CapAnt `Prober` run by `capant::generate` and
  `capant::check`. `matrix/launchers.tsv` is unchanged and `matrix --check` is green. New rule from CapAnt: a `yes` row must name its
  probe, or it grants nothing. `--check` ignores the `inherited_from` column that CapAnt adds to carried-over rows, until the first
  regeneration commit.
- Docs: `docs/MEASUREMENTS.md` records Windows Sandbox host health (package intact; the `WinRT.Runtime` crash is a tool-shell
  launch-context failure, cause unknown).

## 0.0.23

- Fix: `windows-sandbox` reported `TimedOut` for any job that exited with a one-digit code (so every success). `job.cmd` wrote
  `echo %RC%> exit.code`; cmd reads `0>` as a handle redirect, so `exit.code` stayed empty and `run` waited out `time_s`. The
  script now writes `>file echo %RC%`. Host tests run the generated script (`job_script_in`) and include the old form as a
  known-bad control.
- `windows-sandbox`: the guest writes `trace.log` (start, command exit) and `cmd.log` (the command's stdout and stderr);
  `collect` returns them as `windows-sandbox.trace.log` / `.cmd.log`. `run` fails at once if `WindowsSandbox.exe` itself
  exits non-zero before the job finished. `slangc_proof` prints both logs and, on failure, Hyper-V/Sandbox/WER events.
- `github_proof perms <adapter.toml>`: prints the installation's permission names and levels (from the token exchange's
  `permissions`; the token itself is dropped unread). `HttpGithub::installation_permissions`.
- `LICENSE-MIT` holder is now "The antspawn contributors" (CireSnave, decision 263; the company holder waits on ownership).

## 0.0.22

- `github_proof queued <adapter.toml> <owner/repo>`: runs the PR19 `JobSource` live, read-only. First live run is recorded in
  `docs/MEASUREMENTS.md`: refused, the App lacks the repository `Actions: read` permission. `JobSource` was UNVERIFIED until 0.0.23 (see there).

## 0.0.21

- CI: the advisory `portfolio` job is repinned to coderipper `789b012` (coderipper#125). Advisory, not required.

## 0.0.20

- CI: the advisory CodeRipper portfolio check (`portfolio` job, reusable workflow pinned to `c665210`). Not a required check; no rule is silenced.

## 0.0.19

- `HttpGithub` implements `watcher::JobSource`: `queued(repo)` lists the repo's `queued` and `in_progress` runs (a later job of a
  multi-job workflow waits inside a running run), reads each run's jobs (`filter=latest`) and keeps those with status `queued`,
  with their `runs-on` labels. Pages of 100, at most 5 pages per listing. A run that vanishes between the calls (404) is skipped;
  any other failure fails the poll, so the watcher never forgets a job it merely could not see. **verified against GitHub 2026-10-23 (0.0.23: one queued job listed, see MEASUREMENTS)**
  until run live against `antspawn-proof`; the parsing is unit-tested on fixed JSON only.

## 0.0.18

- `antspawn::adapter::watcher`: the watcher loop as pure logic. `Watcher::tick(now)` polls the configured repos through a
  `JobSource`, routes each queued job, mints one just-in-time runner through the `Minter` and hands it to a `Dispatch`.
  One job at a time (nothing polled while busy, at most one mint per tick, oldest first), a job is minted for once, a failed
  mint stays queued under the minter's backoff, unrouted jobs are left alone, a failing repo does not stop the others. The
  JIT config reaches only the dispatcher; `TickReport` holds no secret. Fake source, API and sink in the tests; no real
  `JobSource` or `Dispatch` exists yet (the dispatch waits on board 260), so nothing here touches GitHub.

## 0.0.17

- `account` is certified: `matrix/launchers.tsv` now has `yes` rows for `isolation.no-read-outside`,
  `isolation.no-write-outside`, `clean-slate-next-run`, `kills-runaway-process` and `network.off`, each from a probe that ran
  through the real restricted account on 2026-10-23 (`docs/MEASUREMENTS.md`). `gpu.*` and `concurrent` stay `no`.
  Before this, `spawn run --launcher account` refused every spec.
- New probes: `connect <host:port>` in `spawn-probe`; `Probes::violates_network` (needs a control: the host itself must reach
  the address, else no verdict) and `probes::account::*`, the account launcher's probes, shared by `tests/account.rs` (now five
  tests, adding write-outside and network) and `examples/matrix.rs`. `naive` must reach the network (`tests/negative.rs`).
- `examples/matrix.rs`: `account` is probed where its setup is applied; elsewhere (CI) its committed rows are carried over,
  only if complete and every `yes` names its probe. `windows-sandbox` rows are carried over too (nothing probes it yet).
  `--no-privileged` forces the carry-over path.

## 0.0.16

- `antspawn::adapter::runner_cache`: `ensure_archive` (cached copy reused only if it passes the pin, download renamed
  into place only after it passes, a bad cached file is deleted), `verify_file` (streamed), `unpack` (refuses a
  path-climbing entry and removes the partial result; refuses an existing destination). `examples/runner_fetch.rs` is the
  live proof. New dependency: `zip` (deflate only). The runner is still not started.

## 0.0.15

- `antspawn::adapter::runner_pin`: the pinned `actions/runner` release (v2.338.1 win-x64: URL, size, SHA-256) and
  `verify`, which refuses bytes whose size or digest differ. `docs/RUNNER-PROVENANCE.md` records origin, the three-way
  digest check and licence; the MIT text is kept in `third_party/actions-runner/LICENSE`. Nothing fetches, unpacks or
  starts the runner yet.

## 0.0.14

- `tests/slangc_proof.rs`: the tools directory now carries `msvcp140.dll`, `vcruntime140.dll` and `vcruntime140_1.dll`
  (copied at test time from the host's installed Visual C++ Redistributable, never committed), which `slangc` imports and a
  clean Windows Sandbox lacks (exit 0xC0000135 STATUS_DLL_NOT_FOUND on the desktop run). New red-first test
  `staged_tools_carry_the_visual_cpp_runtime_slangc_imports`. The `MEASURE` line now splits provision/run/collect/destroy and
  prints before the asserts, so a failing run still reports its timings. No launcher code changed.

## 0.0.13

- `antspawn::adapter::github_client`: the real GitHub client (`HttpGithub`, `DpapiKeyProvider`, `app_jwt`, `classify`).
  Signs the App JWT with `ring`, exchanges it for an installation token, lists runner groups/repos/runners, mints a
  JIT runner config, deletes a runner. New `KeyError::Malformed`; `GithubApi` is implemented for `&T`.
  `examples/github_proof.rs` (Windows) is the live proof. New dependencies: `ureq`, `ring`, `base64`, `serde_json`
  (and `windows-sys` Win32_Security_Cryptography). `deny.toml` allows CDLA-Permissive-2.0 (webpki-roots).
  No watcher loop and no runner hand-off yet.

## 0.0.12

- `windows-sandbox`: `destroy` (and `Drop`) retry removing the job's temp root, up to 60 s (10 s on drop). From an
  interactive desktop the VM still held its mapped folders after being closed and removal failed with os error 32.
  `docs/MEASUREMENTS.md`: that run's results.

## 0.0.11

- `examples/measure.rs`: a job that times out or exits non-zero is now an error, not a timing. Before, a launcher whose
  jobs never completed (`windows-sandbox`, 2026-10-22) printed its own time limit as `run`.

## 0.0.10

- `tests/slangc_proof.rs`: the VM leg uses absolute guest paths (`C:\work\...`) and `cwd`, since a Windows Sandbox
  mapped folder cannot be relative; time limit 240 s. `docs/MEASUREMENTS.md`: the `account` slangc proof passes; every
  `windows-sandbox` attempt on this box timed out (see there). No launcher code changed.

## 0.0.9

- `scripts/account-setup.ps1`: no ACL deny on BinDir (its W right includes SYNCHRONIZE, which CreateProcess needs; the task
  failed to launch with 0x80070005) and removes one left by an older run; grants and checks `SeBatchLogonRight` itself.
  Measured live 2026-10-22: after both, the task runs as `antspawn-run`, LastTaskResult 0.

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
