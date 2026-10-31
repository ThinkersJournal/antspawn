# SPDX-License-Identifier: MIT OR Apache-2.0
# SECOND runner account, for negative controls A and B (docs/ACCOUNT-DISPATCH.md). ELEVATED PowerShell as cires.
# Needs scripts/AntSpawnTaskSd.ps1 on main (PR fix/task-sd-check) or the task-SD check gives the same false FAIL as for spawn1.
$s = 'C:\Projects\antspawn\scripts\runner-account-setup.ps1'
$rs = 'C:\Users\cires\antspawn-staging\runner-2.338.1'
# 1. Dry run: changes nothing. CHANGES when applied: local user ghr-spawn2, C:\gh-runner-spawn2 (+runner-src, runs),
#    SeBatchLogonRight for ghr-spawn2, task AntSpawnRunner-spawn2 (pipe antspawn-spawn2).
& $s -Name spawn2 -BinDir 'C:\Program Files\antspawn' -RunnerSource $rs
# 2. Apply; expect 12 PASS and exit=0.
& $s -Apply -Name spawn2 -BinDir 'C:\Program Files\antspawn' -RunnerSource $rs
"exit=$LASTEXITCODE"
# 3. The canary the controls read (only SYSTEM, Administrators and ghr-spawn2 can open it; cires cannot).
Set-Content -Path C:\gh-runner-spawn2\canary -Value 'canary-spawn2' -Encoding ascii
icacls C:\gh-runner-spawn2\canary
# UNDO for spawn2: the UNDO block in PASTE.ps1 with spawn1 replaced by spawn2.
