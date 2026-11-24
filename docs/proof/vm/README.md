# The first live VM job on `antspawn-proof`: files and order (PATH step 4; prepared 2026-11-24, `work-5`)

Nothing here has run. Live running stays held: first live job only on `antspawn-proof`, no registration to aocl or WebHostAnt, nothing against GitHub until the PM says so.

| File | What it is | Who |
|---|---|---|
| `adapter.toml` | one group `antspawn-vm` (label `antspawn-vm`, launcher `hyperv-vm`, repo `antspawn-proof`, `egress_listen = ["10.77.1.1:3128"]`) | cires |
| `vm-host.toml` | the base image, its pinned sha256, `vm_dir`, switch `antspawn-sw1` (read from the config's folder) | cires |
| `vm-proof.yml` | the workflow for `antspawn-proof/.github/workflows/vm-proof.yml`, `workflow_dispatch` only; steps marked "needs rows 39/40" are red until the switches and proxy exist | PM or CireSnave pushes (a lane does not) |

**State read 2026-11-24:** `ThinkersJournal/antspawn-proof` exists, private, default branch `main` (one commit `ff30bd6`, size 0, contents `[]`): **no `.github/workflows` directory and no workflow** (`actions/workflows` total_count 0). So the workflow file is the first thing missing there.

## Order

1. PR for `work-5` merges (the egress listeners, the scripts below).
2. **Elevated, CireSnave (one item, `docs/ELEVATED-39-42.md`):** `broker-v0.ps1 -Undo`, then `broker-pool.ps1 -ProxyPort 3128 -Apply`.
3. As `cires`: `spawn-egress --config <this folder>\adapter.toml --log-dir <dir>`; then `spawn adapter --config <...>\adapter.toml --check` (reads the key, the guest credential, `vm-host.toml`, probes the listener).
4. Push `vm-proof.yml` to `antspawn-proof` (PM). Confirm a runner group lets `antspawn-vm` jobs reach this adapter's registration (a group for `antspawn-proof`, `selected`, that repo only).
5. **Register the adapter task** (cires, not elevated, with Hello where the key needs it; only when the PM says live running is allowed): dry run, then the line below. It starts the adapter at logon and polls GitHub.
6. `workflow_dispatch` on `vm-proof.yml`; the adapter mints a JIT config, boots a differencing child of the base on slot 1, hands the config over PowerShell Direct, the runner runs the job and the VM is deleted. Record in `docs/MEASUREMENTS.md`.

## Registration, dry run and undo

```
# dry run (nothing changes; ran 2026-11-24, output below)
powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\Projects\antspawn\scripts\adapter-task.ps1 -Config C:\Projects\antspawn\docs\proof\vm\adapter.toml
# register (the one line; cires; adds a task that polls GitHub at logon)
powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\Projects\antspawn\scripts\adapter-task.ps1 -Config C:\Projects\antspawn\docs\proof\vm\adapter.toml -Apply
# undo (reads the task back absent)
powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\Projects\antspawn\scripts\adapter-task.ps1 -Config C:\Projects\antspawn\docs\proof\vm\adapter.toml -Undo -Apply
```

Dry-run output (2026-11-24, config path `C:\Projectsntspawn\docs\proofmdapter.toml`): `register AntSpawnAdapter for TUF-LAPTOP\cires, mode=DRY RUN (nothing is changed)`, action `powershell.exe -NoProfile -ExecutionPolicy Bypass -File ...dapter-run.ps1 -Config ...`, trigger at logon, interactive/limited/no stored password, restart 3 times 1 minute apart.

`spawn adapter --check` on this config, read 2026-11-24 from the `work-5` build: org and group lines ok; guest credential decrypts as this user; key loads (1674 bytes, PEM header); effective egress list 8 hosts; `vm host: ... sha256 pinned ... switch antspawn-sw1`; **one problem, as expected: `egress listener 10.77.1.1:3128 is down`** (no pool switch, no proxy yet). Exit 1. The pinned hash is `sha256sum` of `core-vendor-base.vhdx` computed 2026-11-24 (`4e6f8f78...6c0b`); the file's own record elsewhere is not cross-checked.

Task `AntSpawnAdapter`, current user only, interactive logon only, limited (not elevated), no stored password, restart on failure 3 times, never started by the script. `-Apply` is UNRUN (the dry run only); the script says a refused `Register-ScheduledTask` would change nothing and is then an elevation item for the PM.

## What the workflow proves, and what it does not

* Positive: the baked toolchain (`cargo`, `git`) is in the guest; a fresh child has no sentinel from an earlier run; only drive `C:` and no SMB mapping (no host mounts).
* Controls (rows 39/40): an allowed host (`api.github.com`) is reached through the proxy; `example.com` is refused; a direct connection that skips the proxy goes nowhere; nine named host ports are closed while 3128 answers (the positive control for the scan).
* **Not covered:** a job that tries to reach another slot's switch (needs two guests); any TLS-fronting attempt (the proxy's `--check-sni` is a flag, off here); the runner's own update hosts (deliberately not on the list); the guest's Administrator account (the image has one fixed credential, board 488 "A"): the job runs as an administrator inside a VM that is destroyed afterwards, so the boundary is the VM and the proxy, not the account.
