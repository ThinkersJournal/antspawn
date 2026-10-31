# The account-backed 3-run proof: files and order (ROADMAP row 21 slice d)

Design and the controls: `docs/ACCOUNT-DISPATCH.md`. Nothing in this folder has been run end to end; see `docs/MEASUREMENTS.md` for what has.

| File | What it is | Who runs it |
|---|---|---|
| `PASTE-spawn1.ps1` | install the shim, dry-run, `-Apply -Name spawn1`, read-back, UNDO (CireSnave ran the apply 2026-10-31) | done |
| `PASTE-spawn2.ps1` | the SECOND runner account + canary, for controls A and B | **CireSnave, elevated** (dry-run first) |
| `make-dpapi-key.ps1` | PEM -> the DPAPI key file; only if the key is missing or rotated | cires, not elevated, dry-run first |
| `adapter.toml` | two account groups, one per runner account | cires |
| `proof.yml` | the workflow the PM pushes to `antspawn-proof/.github/workflows/` | PM pushes; `workflow_dispatch` |

## Order

1. **Merge** #59 (`spawn adapter`) and #61 (task-SD check); build `spawn`.
2. **Key (no action expected).** The DPAPI file was made on 2026-10-23 and used for the live probe in `MEASUREMENTS.md`:
   `C:\Users\cires\AppData\Local\antspawn\github-app\antspawn-runner.key.dpapi` (7148 bytes; read as cires 2026-10-31, listing only, not decrypted).
   If it is missing or the key was rotated: App settings > Private keys > Generate a private key (downloads a `.pem`), then in a normal PowerShell as cires
   `.\docs\proof\make-dpapi-key.ps1 -Pem <the .pem>` (dry run), then again with `-Apply`. It writes the hex-of-DPAPI-blob file, readable only by cires and SYSTEM, decrypts it to
   check, and deletes the `.pem`. Only that Windows account can decrypt it. Then revoke the old key in the App settings.
3. **GitHub, PM:** create runner group `antspawn-account-2` (selected, only `ThinkersJournal/antspawn-proof`); `antspawn-account` (id 3) exists. Push `proof.yml`.
4. **Elevated, CireSnave:** `PASTE-spawn2.ps1` (dry-run, apply, canary). The only elevated step left.
5. **Standard user:** `spawn adapter --config docs/proof/adapter.toml --check`; `cargo run --example github_proof -- probe docs/proof/adapter.toml`
   (contacts GitHub read-only); then dispatch the workflow and run `spawn adapter ... --once` for each queued job; three full runs; record in `MEASUREMENTS.md`.

## What the workflow proves, and what it does not

* Positive: runs as `ghr-spawn1`, reads AOCL, no sentinel from an earlier run (fresh copy), leaves one.
* Control A: `ghr-spawn1` reading `C:\gh-runner-spawn2\canary` must raise UnauthorizedAccess (any other failure fails the step: not a proven denial);
  the same read as `ghr-spawn2` must return `canary-spawn2` (the canary exists and its owner can read it).
* Control B: `ghr-spawn1` writing into `C:\gh-runner-spawn2\runs` must raise UnauthorizedAccess.
* **Not covered:** the account's REAL profile is not wiped between runs (the setup script says so); a job writing there by absolute path leaves files. The launcher-user
  "naive" control of the design doc is dropped: the launcher (cires) is also denied `C:\gh-runner-*`, which `MEASUREMENTS.md` records as its own check.
* Outbound network is open on this path (board 341 A); the firewall limit (board 346) is not done.
