<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Redeploying `spawn-runner.exe`

The account launcher's runner is `C:\Program Files\antspawn\spawn-runner.exe`, run by the `AntSpawnRunner` task as `antspawn-run`.
A new host verb (session ops, `put`/`get`/`list`, bind's `root`) only works live after this file is replaced. **The swap is elevated**:
that folder is writable by Administrators only (`Users` have read and execute), and the account must not be able to write it.
No ACL, account or task changes; it is one file, hash-checked before and read back after. Last done 2026-11-08 (board 399).

## Steps

1. **Not elevated (PM or lane):** get the CI build of the commit to deploy. CI uploads artifact `spawn-runner-windows-x64`
   (`spawn-runner.exe`, `spawn-runner.exe.sha256`, and the same for `spawn-wfp`):
   `gh run list --repo ThinkersJournal/antspawn --branch main --workflow ci.yml --limit 3` (take the run whose head is the commit), then
   `gh run download <run-id> --repo ThinkersJournal/antspawn -n spawn-runner-windows-x64 -D C:\Users\cires\antspawn-staging\runner-<sha8>`.
   Record the commit and the sha256 in the request to CireSnave.
2. **Elevated (CireSnave's paste; a Windows Hello or UAC prompt is the approval):**
   `& C:\Projects\antspawn\docs\proof\PASTE-redeploy-runner.ps1 -Source C:\Users\cires\antspawn-staging\runner-<sha8>\spawn-runner.exe`
   It refuses an artifact whose hash differs from its `.sha256` file, refuses while a `spawn-runner` process runs, keeps
   `spawn-runner.exe.bak-pre-<sha8>`, copies to `.new`, verifies, moves over, reads the hash back and prints the ACL.
3. **Not elevated:** read it back yourself: `(Get-FileHash 'C:\Program Files\antspawn\spawn-runner.exe').Hash` equals the CI hash; then
   `cargo test --test account_session` (`RUST_TEST_THREADS=1`) must pass without printing `UNVERIFIED`. Record both in `docs/MEASUREMENTS.md`.

Rollback: copy the `.bak-pre-<sha8>` file back the same way (elevated). Nothing else holds state.
Unmeasured: whether the task picks up a swapped file mid-run (the script refuses to run while a runner process lives, so it is not tried).
