# SPDX-License-Identifier: MIT OR Apache-2.0
# Run in an ELEVATED PowerShell as cires. Nothing here is run by a lane.
# 1. Install the shim where the restricted account can execute it but not write it.
New-Item -ItemType Directory -Force 'C:\Program Files\antspawn' | Out-Null
Copy-Item C:\Users\cires\antspawn-staging\bin\spawn-jit-shim.exe 'C:\Program Files\antspawn\' -Force
Get-FileHash 'C:\Program Files\antspawn\spawn-jit-shim.exe' | % Hash   # expect 3D83E4CB784D967E4D0303FB844DFDAE60C2EDBD47CCC47A33DF76094CF418AF (built from main 0e10cbe)

# 2. Dry run first (changes nothing, prints each step).
$s = 'C:\Projects\antspawn\scripts\runner-account-setup.ps1'
& $s -Name spawn1 -BinDir 'C:\Program Files\antspawn' -RunnerSource C:\Users\cires\antspawn-staging\runner-2.338.1

# 3. Apply (creates user ghr-spawn1, C:\gh-runner-spawn1, task AntSpawnRunner-spawn1; prints PASS/FAIL per property; exit 1 on any FAIL).
& $s -Apply -Name spawn1 -BinDir 'C:\Program Files\antspawn' -RunnerSource C:\Users\cires\antspawn-staging\runner-2.338.1
"exit=$LASTEXITCODE"

# 4. Read-back by hand (independent of the script's own checks).
Get-LocalUser ghr-spawn1 | ft Name, Enabled
Get-ScheduledTask AntSpawnRunner-spawn1 | % { $_.Principal.UserId; $_.Actions[0].Execute; $_.Actions[0].Arguments }
icacls C:\gh-runner-spawn1
Get-ChildItem C:\gh-runner-spawn1 -Force | Select -Expand Name   # expect: runner-src, runs

# UNDO (elevated). Removes the task, the account, the root, and its batch-logon right.
Unregister-ScheduledTask -TaskName AntSpawnRunner-spawn1 -Confirm:$false
$sid = (Get-LocalUser ghr-spawn1).SID.Value
$f = "$env:TEMP\undo.inf"; $db = "$env:TEMP\undo.sdb"
secedit /export /cfg $f /areas USER_RIGHTS | Out-Null
$l = (Get-Content $f | ? { $_ -match '^SeBatchLogonRight' }) -replace '^[^=]*=\s*', ''
$keep = ($l.Split(',') | % { $_.Trim() } | ? { $_ -and $_ -ne "*$sid" }) -join ','
Set-Content $f -Encoding unicode "[Unicode]`r`nUnicode=yes`r`n[Version]`r`nsignature=`"`$CHICAGO`$`"`r`nRevision=1`r`n[Privilege Rights]`r`nSeBatchLogonRight = $keep`r`n"
secedit /configure /db $db /cfg $f /areas USER_RIGHTS | Out-Null
Remove-LocalUser ghr-spawn1
takeown /F C:\gh-runner-spawn1 /R /D Y | Out-Null
icacls C:\gh-runner-spawn1 /grant 'Administrators:(OI)(CI)F' /T | Out-Null
Remove-Item C:\gh-runner-spawn1 -Recurse -Force
Remove-Item 'C:\Program Files\antspawn' -Recurse -Force   # only if nothing else uses it
Remove-Item $f, $db -ErrorAction SilentlyContinue
