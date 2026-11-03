# SPDX-License-Identifier: MIT OR Apache-2.0
# ONE paste, ELEVATED PowerShell as cires. Nothing here is run by a lane.
# Registers the Windows Sandbox package (wsb.exe) for the three existing runner accounts by re-running their setup scripts
# (each now resets the account's password, registers the package AS the account, re-registers its task with the new password,
# and reads everything back). Needs this change merged and C:\Projects\antspawn fast-forwarded to it.
# CAUTION: account-setup.ps1 re-applies its deny ACE on C:\Projects to every file under it: that can take minutes.
# A running AntSpawnRunner / AntSpawnRunner-spawn* task is replaced by the re-registration: let jobs finish first.
$ErrorActionPreference = 'Continue'
$sc  = 'C:\Projects\antspawn\scripts'
$bin = 'C:\Program Files\antspawn'
$rs  = 'C:\Users\cires\antspawn-staging\runner-2.338.1'
$log = "C:\Users\Public\wsb-register-$(Get-Date -Format yyyyMMdd-HHmmss).log"
Start-Transcript -Path $log | Out-Null
$steps = [ordered]@{}
function Run([string]$Label, [scriptblock]$Do) {
    Write-Host "`n===== $Label"
    $global:LASTEXITCODE = 0
    & $Do
    $steps[$Label] = if ($LASTEXITCODE -eq 0) { 'PASS' } else { "FAIL (exit $LASTEXITCODE)" }
}

# 1. Dry runs first (change nothing; each prints the registration step as WOULD).
Run 'dry-run ghr-spawn1' { & "$sc\runner-account-setup.ps1" -Name spawn1 -BinDir $bin -RunnerSource $rs }
Run 'dry-run ghr-spawn2' { & "$sc\runner-account-setup.ps1" -Name spawn2 -BinDir $bin -RunnerSource $rs }
Run 'dry-run antspawn-run' { & "$sc\account-setup.ps1" -BinDir $bin }

# 2. Apply. Each script prints PASS/FAIL per property, including the package InstallState and wsb.exe; its exit code is non-zero on any FAIL.
Run 'apply ghr-spawn1' { & "$sc\runner-account-setup.ps1" -Apply -Name spawn1 -BinDir $bin -RunnerSource $rs }
Run 'apply ghr-spawn2' { & "$sc\runner-account-setup.ps1" -Apply -Name spawn2 -BinDir $bin -RunnerSource $rs }
Run 'apply antspawn-run' { & "$sc\account-setup.ps1" -Apply -BinDir $bin }

# 3. Independent read-back, not the scripts' own checks: one row per account.
$rows = foreach ($u in 'ghr-spawn1', 'ghr-spawn2', 'antspawn-run') {
    $sid = (Get-LocalUser $u -ErrorAction SilentlyContinue).SID.Value
    $state = ''
    foreach ($p in @(Get-AppxPackage -AllUsers -Name MicrosoftWindows.WindowsSandbox)) {
        foreach ($ui in @($p.PackageUserInformation)) { if ("$($ui.UserSecurityId)" -match [regex]::Escape("$sid")) { $state = "$($ui.InstallState)" } }
    }
    $prof = (Get-CimInstance Win32_UserProfile -Filter "SID='$sid'" -ErrorAction SilentlyContinue).LocalPath
    $wsb = if ($prof) { Join-Path $prof 'AppData\Local\Microsoft\WindowsApps\wsb.exe' } else { '' }
    $t = Get-ScheduledTask | Where-Object { $_.Principal.UserId -match "$u$" } | Select-Object -First 1
    [pscustomobject]@{ Account = $u; Sid = $sid; PackageState = $state; WsbExe = [bool]($wsb -and (Test-Path $wsb)); Task = $t.TaskName; TaskUser = $t.Principal.UserId }
}
$rows | Format-Table -AutoSize | Out-String -Width 200
Write-Host "`n===== SUMMARY"
$steps.GetEnumerator() | ForEach-Object { '{0,-6} {1}' -f $_.Value, $_.Key }
Stop-Transcript | Out-Null
"transcript: $log   (give this file to the PM; it holds no password)"

# UNDO for the package only (elevated). Un-registers it per account; the accounts, tasks and roots stay. The password reset cannot be
# undone and need not be: nobody held the old one, and each task was re-registered with the new one.
#   foreach ($u in 'ghr-spawn1','ghr-spawn2','antspawn-run') {
#       $sid = (Get-LocalUser $u).SID.Value
#       Get-AppxPackage -AllUsers -Name MicrosoftWindows.WindowsSandbox | ForEach-Object { Remove-AppxPackage -Package $_.PackageFullName -User $sid }
#   }
# Do NOT remove it for 'cires': that is the interactive user's own registration. Whole-account undo: the UNDO block of PASTE-spawn1.ps1.
