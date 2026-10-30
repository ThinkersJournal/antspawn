# AntSpawn design (slice 1)

1. **Job spec** (TOML, strict): `[job]` name/cmd/cwd/env/secrets, `[[mount]]` host/guest/mode (`ro`, `rw`,
   `scratch`), `[limits]`, `[needs]` network/gpu/tools/require/prefer, `[[output]]` guest->name.
   `secrets` are **names only**; this version implements none and refuses a spec that lists one.
2. **Needs** = `require` (default: the isolation set) + `network.off` when `network = "off"` + the GPU
   capability when `gpu` is set.
3. **Launcher** trait: `id()`, `provision(spec) -> Environment`; **Environment**: `run`, `collect`,
   `destroy` (also on `Drop`). Launchers never elevate and never create accounts.
4. **Registry**: `providers(needs)` lists every launcher, satisfying first, in `prefer` order, naming each
   unmet need; `open_named(id, needs)` re-checks and refuses with the reason.
5. **Matrix**: PersistAnt's nine-column TSV in `matrix/`; a capability is on only for a `capability` row
   with `enabled = yes`. Parsing is strict.
6. **Tests**: types, parser and the `naive` launcher run on every CI leg. Tests that need a privileged
   account, an optional Windows feature or a runner (`account`, `windows-sandbox`) are runner-only and
   are **UNVERIFIED** in CI until a runner exists; they are never `#[ignore]`d. Local results are
   recorded with date and ref in `docs/MEASUREMENTS.md`.
7. **`account` launcher** (no password anyone holds): the job runs AS the local account `antspawn-run`, whose
   random password is used once by `scripts/account-setup.ps1 -Apply` (CireSnave, elevated, dry-run by default,
   reads state back) to register the scheduled task `AntSpawnRunner` (`spawn-runner serve`) and is then
   discarded. The launcher never elevates and never creates anything; a missing piece is a loud
   `NotProvisioned`.
   * **Copy, not bind.** The account has no access to the launcher user's files (and `C:\Projects` is ACL-denied
     to it), so a mount cannot be a bind: `ro` sources are copied IN (`spool/<id>/in/<n>`, then into the
     runner's work dir) and declared `[[output]]`s are copied OUT (`results/<id>/out/<name>`). `rw` mounts are
     refused. Guest paths (mounts, cwd, outputs) are relative to the work dir; absolute, drive and `..` paths
     are refused.
   * **Flow.** Launcher: refuse what the runner would refuse, spool `spec.toml` + inputs, `schtasks /Run`,
     poll `results/<id>/exit.code`, copy outputs, delete its spool and results dirs. Runner: for each pending
     spool dir, start `spawn-runner exec <id>` which joins a kill-on-close Job Object, copies inputs in, runs
     the command (children inherit the job), copies outputs out, writes `result.tmp`, then terminates the job.
     `serve` wipes the work dir and only then renames `result.tmp` to `exit.code`, so that file's presence
     means the whole process tree is dead and the work dir is gone.
   * **Authority.** Only the launcher user writes the spool (ACL); `runner.toml` (admin-written) caps
     `time_s` and `ram_mb`, and the runner re-checks every spec so it does not trust the launcher. `cpu`,
     `gpu` and `network = "on"` are refused (not provided). The account's outbound traffic is firewalled.
   * **One job at a time.** The task ignores a second start while running; `serve` rescans until the spool is
     empty. `concurrent` is never claimed.
   * **Open (measure, do not guess):** whether a non-admin `schtasks /Run` works through the task's security
     descriptor on this machine, versus an ACL'd named pipe. See `docs/MEASUREMENTS.md`.

## `windows-sandbox`: how it starts and who the job runs as

It drives `wsb.exe` (the Windows Sandbox package's CLI, which starts with package identity; the System32
`WindowsSandbox.exe <file>.wsb` stub does not, and its client crashes). `wsb start -c <config>` creates the VM
(network and vGPU off, clipboard/printer/audio/video redirection off, only the declared folders mapped, `in` read-only),
`wsb exec -r System` runs the generated `job.cmd`, `wsb stop --id` ends it. The job therefore runs as SYSTEM inside the
throwaway VM: `ExistingLogin` needs an interactive guest logon, which a headless start never creates (0x80070520), and
nothing the job does as SYSTEM outlives the VM or reaches the host except through the writable `out` mapping and the
declared `rw` mounts. A launcher that is killed outright leaves its VM; every run first stops the sandboxes recorded in
`%TEMP%\antspawn-wsb-<id>.owner` whose owning process is gone (only its own, by id). `run` also fails within about 2 s,
naming the event, when the Application log shows a `WindowsSandbox*.exe` crash after the request.

### Smart App Control inside the guest (board 260, PM ruling A, 2026-10-24)

> **Status 2026-10-29: the claim below is NOT currently re-proven.** It was measured on 2026-10-24 against sandbox layer `1ef1ee21` (created
> 2026-10-22). Since the layer was replaced on 2026-10-28 the raw-guest control no longer holds: a fresh guest reads
> `VerifiedAndReputablePolicyState = 0x2` with the *Evaluation* policy active and an unsigned exe starts every time (see MEASUREMENTS,
> "Stale control"). Read the paragraph as what was true on 2026-10-24. The switch-off is harmless but its need is unproven today.

The Windows Sandbox image enforces Smart App Control (`VerifiedAndReputableDesktop`). It lets an unsigned executable
start once and then blocks it, and it blocks unsigned managed DLLs outright, so the GitHub runner (14 unsigned files
in v2.338.1, among them `Runner.Listener.dll`) cannot run in it. The first thing `job.cmd` does, as SYSTEM inside the
guest, is write `VerifiedAndReputablePolicyState = 0` under the guest's `CI\Policy` key and run `CiTool --refresh`,
repeating until it reads back `0x0` (the guest's own evaluation can undo one write while it settles after boot) and
recording `[sac-before]` / `[sac-after tries=N]` in the guest `trace.log`.

- **The hypervisor, not Smart App Control, is the isolation boundary for jobs.** The control that is relaxed is
  defence in depth inside a disposable VM that already runs untrusted jobs; reads of host paths, writes outside the
  mapped folders and (except where the 260 ruling allows it) the network stay off, and the probes that certify the
  launcher run with the switch-off in place.
- **Never on the host.** The commands exist only in the text `job_script` puts into the mapped `in` folder as `job.cmd`,
  and only `wsb exec` runs that file. Every host-side use of the script builder passes no prelude, `job_script` is
  private, and a unit test counts the uses. `tests/windows_sandbox.rs` compares the host's state before and after a full
  run: the `VerifiedAndReputablePolicyState` value and the name and SHA-256 of every file in
  `CodeIntegrity\CiPolicies\Active` (`CiTool --list-policies` needs elevation on the host, which nothing here uses).
- The control for the test is a raw `wsb` VM with no switch-off, where the unsigned exe's second start exits 1073751882.
