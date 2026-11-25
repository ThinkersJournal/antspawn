# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Undo of scripts\runner-account-setup.ps1 for ONE runner account `ghr-<Name>` (and nothing else).
  Elevated for -Apply, run by the machine owner, never by a lane. DRY-RUN BY DEFAULT: without -Apply it only reads
  and prints what it would remove and what it would leave.

.DESCRIPTION
  -Apply removes, for `ghr-<Name>` only: task `AntSpawnRunner-<Name>`, the account's `Log on as a batch job` right
  (only its own SID is taken out of the list), the local user, and the root `C:\gh-runner-<Name>` (ownership taken,
  Administrators granted, then deleted). Then it READS BACK each property as GONE/STILL THERE.
  It leaves `C:\Program Files\antspawn` (shared with other accounts), the other runner accounts, `antspawn-run`, every
  firewall rule and every other task. It refuses a -Name outside ^[a-z0-9-]{1,16}$ and any account whose name is not
  `ghr-<Name>`. A running task is stopped first; a logged-on session for the account stops the removal (reported).
#>
[CmdletBinding()]
param(
    [switch]$Apply,
    [Parameter(Mandatory)][ValidatePattern('^[a-z0-9-]{1,16}$')][string]$Name
)
$ErrorActionPreference = 'Stop'
$Account = "ghr-$Name"
$TaskName = "AntSpawnRunner-$Name"
$Root = "C:\gh-runner-$Name"
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if ($Apply -and -not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this in an elevated PowerShell for -Apply (the dry run needs none).'
}
Write-Host ("mode: " + $(if ($Apply) { 'APPLY' } else { 'DRY RUN (nothing is changed)' }))

$user = Get-LocalUser -Name $Account -ErrorAction SilentlyContinue
$taskObj = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
$sid = if ($user) { $user.SID.Value } else { $null }
$batch = $false
$batchKnown = $false
if ($sid) {
    $f = Join-Path $env:TEMP "undo-read-$Name.inf"
    & secedit.exe /export /cfg $f /areas USER_RIGHTS | Out-Null
    $rights = @(Get-Content -LiteralPath $f -ErrorAction SilentlyContinue)
    $batchKnown = $rights.Count -gt 0   # secedit /export needs elevation: without it the file is absent, and 'not granted' would be a guess
    $line = ($rights | Where-Object { $_ -match '^SeBatchLogonRight' }) -join ''
    $batch = $line -match [regex]::Escape("*$sid")
    Remove-Item -LiteralPath $f -ErrorAction SilentlyContinue
}
Write-Host ("FOUND  task {0}: {1}" -f $TaskName, $(if ($taskObj) { "present, $($taskObj.State)" } else { 'absent' }))
Write-Host ("FOUND  account {0}: {1}" -f $Account, $(if ($user) { "present, SID $sid" } else { 'absent' }))
Write-Host ("FOUND  batch-logon right for it: {0}" -f $(if (-not $batchKnown) { 'UNKNOWN (reading it needs elevation; -Apply reads it)' } elseif ($batch) { 'granted' } else { 'not granted' }))
Write-Host ("FOUND  root {0}: {1}" -f $Root, $(if (Test-Path -LiteralPath $Root) { 'present' } else { 'absent' }))
Write-Host "PLAN   stop and unregister the task; remove only SID $sid from SeBatchLogonRight; Remove-LocalUser $Account; take ownership of and delete $Root"
Write-Host "LEAVES C:\Program Files\antspawn, every other account and task, every firewall rule"
if (-not $Apply) { Write-Host 'Dry run complete. -Apply does the above and reads each property back.'; return }

if ($taskObj) {
    Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
}
if ($user) {   # elevated here: the right is re-read inside, so a dry run's unknown does not matter
    $batch = $true
    $f = Join-Path $env:TEMP "undo-$Name.inf"; $db = Join-Path $env:TEMP "undo-$Name.sdb"
    & secedit.exe /export /cfg $f /areas USER_RIGHTS | Out-Null
    $l = ((Get-Content -LiteralPath $f | Where-Object { $_ -match '^SeBatchLogonRight' }) -replace '^[^=]*=\s*', '')
    $keep = ($l.Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ -and $_ -ne "*$sid" }) -join ','
    Set-Content -LiteralPath $f -Encoding Unicode ("[Unicode]`r`nUnicode=yes`r`n[Version]`r`nsignature=`"`$CHICAGO`$`"`r`nRevision=1`r`n[Privilege Rights]`r`nSeBatchLogonRight = $keep`r`n")
    & secedit.exe /configure /db $db /cfg $f /areas USER_RIGHTS | Out-Null
    Remove-Item -LiteralPath $f, $db -ErrorAction SilentlyContinue
}
if ($user) { Remove-LocalUser -Name $Account }
if (Test-Path -LiteralPath $Root) {
    & takeown.exe /F $Root /R /D Y | Out-Null
    & icacls.exe $Root /grant 'Administrators:(OI)(CI)F' /T | Out-Null
    Remove-Item -LiteralPath $Root -Recurse -Force
}

# read back
$bad = 0
function Check([string]$what, [bool]$gone) {
    if ($gone) { Write-Host "PASS   ${what}: GONE" } else { Write-Host "FAIL   ${what}: STILL THERE"; $script:bad++ }
}
Check "task $TaskName" (-not (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue))
Check "account $Account" (-not (Get-LocalUser -Name $Account -ErrorAction SilentlyContinue))
Check "root $Root" (-not (Test-Path -LiteralPath $Root))
if ($sid) {
    $f = Join-Path $env:TEMP "undo-back-$Name.inf"
    & secedit.exe /export /cfg $f /areas USER_RIGHTS | Out-Null
    $line = (Get-Content -LiteralPath $f | Where-Object { $_ -match '^SeBatchLogonRight' }) -join ''
    Remove-Item -LiteralPath $f -ErrorAction SilentlyContinue
    Check "batch-logon right for SID $sid" (-not ($line -match [regex]::Escape("*$sid")))
}
if ($bad -gt 0) { Write-Host "FAILED ($bad)"; exit 1 }
Write-Host 'DONE   all properties read back gone'
