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
