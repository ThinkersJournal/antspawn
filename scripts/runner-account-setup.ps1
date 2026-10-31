# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  One-time, elevated setup for ONE account-backed Actions runner (ROADMAP row 21, docs/ACCOUNT-DISPATCH.md).
  Run by the machine owner, never by a lane. FAILED on its first real run (2026-10-31, 0.0.58: `New-LocalUser -Description`
  was 59 characters, the limit is 48; and the error left the exit code 0). Fixed in 0.0.59; NOT YET COMPLETED anywhere.

.DESCRIPTION
  Creates the restricted local account `ghr-<Name>` (no group beyond Users), its private root
  `C:\gh-runner-<Name>` (only that account, SYSTEM and Administrators; the launcher user has NO access), a copy
  of the pinned runner as a TEMPLATE the account can read but not write (`<root>\runner-src`), and a scheduled
  task that runs `spawn-jit-shim --pipe <PipeName> --allow-sid <launcher SID> --fresh <root>\runner-src
  <root>\runs run.cmd` AS that account: every job runs in a new copy under `<root>\runs` that is deleted, after
  its processes are ended, when the job is over. The launcher user may run the task and nothing else.
  The account's password is random, used once to register the task, then discarded.
  DRY-RUN BY DEFAULT: without -Apply it changes nothing and prints what it would do. With -Apply it does each
  step, then READS THE STATE BACK and prints PASS/FAIL per property; the exit code is non-zero on any FAIL.
  Safe to re-run. Run it once per runner account; each account gets its own root and its own pipe.

  What it does NOT do (so do not read a PASS as more than it says):
  * It does not touch the firewall. Board 341 ruled outbound network allowed on this path; limiting it to
    GitHub's hosts is not expressible as Windows Firewall rules (a block rule overrides an allow rule, and
    rules take addresses, not names). How to limit is CireSnave's call, still open.
  * The wipe between runs is the shim's `--fresh` (design item 4), tested locally as the current user only.
    The runner's TEMP and profile variables point into the copy; a job writing the account's REAL profile by
    absolute path still leaves files for the next job, and this script does not clean that.
  * It does not start the task: the shim waits for a config on its pipe and would just time out.
  * It does not prove isolation. The negative controls A and B (docs/ACCOUNT-DISPATCH.md) do, after -Apply.

.PARAMETER Apply         Make the changes (needs an elevated PowerShell).
.PARAMETER Name          Runner name, 1-16 chars of a-z 0-9 '-'; the account is ghr-<Name>.
.PARAMETER BinDir        Folder holding spawn-jit-shim.exe. NOT under C:\Users; not writable by the account.
.PARAMETER RunnerSource  The pinned Actions runner folder (read-only cache); copied into the account's root.
.PARAMETER LauncherUser  The user that delivers the config and may trigger the task. Default: current user.
.PARAMETER PipeName      Pipe the shim creates. Default: antspawn-<Name>.
#>
[CmdletBinding()]
param(
    [switch]$Apply,
    [Parameter(Mandatory)][ValidatePattern('^[a-z0-9-]{1,16}$')][string]$Name,
    [Parameter(Mandatory)][string]$BinDir,
    [Parameter(Mandatory)][string]$RunnerSource,
    [string]$LauncherUser = "$env:USERDOMAIN\$env:USERNAME",
    [string]$PipeName = "antspawn-$Name"
)
$ErrorActionPreference = 'Stop'

# Any terminating error below ends in the catch at the bottom: it prints FAILED and exits 1. Without it, a
# `throw` leaves $LASTEXITCODE untouched, so `& script.ps1; $LASTEXITCODE` read 0 after a failed run
# (seen on the first real run, 2026-10-31). Errors raised while binding the parameters (a bad -Name) happen
# before this point and are PowerShell's own.
try {

. (Join-Path $PSScriptRoot 'AntSpawnTaskSd.ps1')
$AccountName = "ghr-$Name"
# New-LocalUser limits -Description to 48 characters, whatever -Name is (the first real run failed on 59).
$AccountDescription = 'AntSpawn runner; nobody holds its password'
$TaskName = "AntSpawnRunner-$Name"
$Root = "C:\gh-runner-$Name"
$BinDir = [IO.Path]::GetFullPath($BinDir)
$Shim = Join-Path $BinDir 'spawn-jit-shim.exe'
$Template = Join-Path $Root 'runner-src'
$RunCmd = Join-Path $Template 'run.cmd'
$usersRoot = Join-Path $env:SystemDrive 'Users'
if ($BinDir.StartsWith($usersRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "BinDir $BinDir is under ${usersRoot}: the restricted account cannot execute files in a user profile. Use e.g. 'C:\Program Files\antspawn'."
}
if ($PipeName -notmatch '^[A-Za-z0-9._-]{1,64}$') { throw "PipeName must be 1-64 chars of A-Z a-z 0-9 . _ -" }

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
$elevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($Apply -and -not $elevated) { throw 'Run this in an elevated PowerShell for -Apply (dry-run needs no elevation).' }
if (-not (Test-Path $Shim)) { throw "spawn-jit-shim.exe not found at $Shim" }
if (-not (Test-Path (Join-Path $RunnerSource 'run.cmd'))) { throw "no run.cmd in RunnerSource $RunnerSource (not an Actions runner folder)" }

$results = [System.Collections.Generic.List[object]]::new()
function Step([string]$What, [scriptblock]$Do) {
    if ($Apply) { Write-Host "APPLY  $What"; & $Do } else { Write-Host "WOULD  $What" }
}
function Check([string]$Property, [scriptblock]$Test) {
    $ok = $false
    try { $ok = [bool](& $Test) } catch { $ok = $false }
    $results.Add([pscustomobject]@{ Property = $Property; Ok = $ok })
    Write-Host ("{0,-5} {1}" -f ($(if ($ok) { 'PASS' } else { 'FAIL' })), $Property)
}
function Acl([string]$Path, [string[]]$IcaclsArgs) {
    & icacls.exe $Path @IcaclsArgs | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls $Path $IcaclsArgs failed ($LASTEXITCODE)" }
}
function SidOf([string]$User) { ([Security.Principal.NTAccount]$User).Translate([Security.Principal.SecurityIdentifier]).Value }

# --- the account (password: random, used once below, never stored) ---------------------------------
$account = Get-LocalUser -Name $AccountName -ErrorAction SilentlyContinue
$task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
$plain = $null
if ($Apply -and ((-not $account) -or (-not $task))) {
    # Local password policy wants 3 of 4 character classes; base64 usually has them, so insist on it rather than hope.
    do {
        $bytes = [byte[]]::new(48)
        $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
        $rng.GetBytes($bytes)   # works in Windows PowerShell 5.1 too ([RandomNumberGenerator]::Fill needs .NET Core)
        $rng.Dispose()
        $plain = [Convert]::ToBase64String($bytes)
    } until (($plain -cmatch '[A-Z]') -and ($plain -cmatch '[a-z]') -and ($plain -match '[0-9]'))
}
Step "create local user $AccountName (random password, never expires, cannot change it, no groups added)" {
    if (-not $account) {
        New-LocalUser -Name $AccountName -Password (ConvertTo-SecureString $plain -AsPlainText -Force) `
            -PasswordNeverExpires -UserMayNotChangePassword -AccountNeverExpires `
            -Description $AccountDescription | Out-Null
    } elseif (-not $task) {
        Set-LocalUser -Name $AccountName -Password (ConvertTo-SecureString $plain -AsPlainText -Force)
    }
}
$account = Get-LocalUser -Name $AccountName -ErrorAction SilentlyContinue
$sid = if ($account) { $account.SID.Value } else { '<account SID>' }
$launcherSid = if ($Apply) { SidOf $LauncherUser } else { '<launcher SID>' }

# --- the private root and the runner copy ----------------------------------------------------------
Step "create $Root; ACL: SYSTEM + Administrators full, $AccountName modify, nobody else (not $LauncherUser)" {
    New-Item -ItemType Directory -Force $Root | Out-Null
    Acl $Root @('/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', "*${sid}:(OI)(CI)M")
}
Step "create $Root\runs (the account writes: one new copy per job lives and dies here)" {
    New-Item -ItemType Directory -Force "$Root\runs" | Out-Null
}
Step "copy the pinned runner from $RunnerSource to $Template (once; robocopy exit < 8 is success); then the account may only read and execute it" {
    if (-not (Test-Path $RunCmd)) {
        & robocopy.exe $RunnerSource $Template /E /NFL /NDL /NJH /NJS | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "robocopy failed ($LASTEXITCODE)" }
    }
    # The template must not be writable by the account, or one job could poison every later copy.
    Acl $Template @('/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', "*${sid}:(OI)(CI)RX")
}

# --- 'Log on as a batch job' (the task cannot start without it) ------------------------------------
function Get-BatchRightSids {
    $f = Join-Path ([IO.Path]::GetTempPath()) ("antspawn-sec-" + [guid]::NewGuid().ToString('N') + '.inf')
    try {
        & secedit.exe /export /cfg $f /areas USER_RIGHTS | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "secedit /export failed ($LASTEXITCODE)" }
        $line = Get-Content $f | Where-Object { $_ -match '^SeBatchLogonRight\s*=' }
        if ($line) {
            ($line -replace '^[^=]*=\s*', '').Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ } | ForEach-Object {
                if ($_.StartsWith('*')) { $_ } else { '*' + (SidOf $_) }
            }
        } else { @() }
    } finally { Remove-Item $f -ErrorAction SilentlyContinue }
}
Step "grant $AccountName the right 'Log on as a batch job' (SeBatchLogonRight)" {
    $have = @(Get-BatchRightSids)
    if ($have -notcontains "*$sid") {
        $inf = Join-Path ([IO.Path]::GetTempPath()) ("antspawn-sec-" + [guid]::NewGuid().ToString('N') + '.inf')
        $db = [IO.Path]::ChangeExtension($inf, '.sdb')
        try {
            $sids = (@($have) + "*$sid") -join ','
            Set-Content -Path $inf -Encoding unicode -Value "[Unicode]`r`nUnicode=yes`r`n[Version]`r`nsignature=`"`$CHICAGO`$`"`r`nRevision=1`r`n[Privilege Rights]`r`nSeBatchLogonRight = $sids`r`n"
            & secedit.exe /configure /db $db /cfg $inf /areas USER_RIGHTS | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "secedit /configure failed ($LASTEXITCODE)" }
        } finally { Remove-Item $inf, $db -ErrorAction SilentlyContinue }
    }
}

# --- the task, run AS the account; the launcher may run it but not change it -----------------------
$shimArgs = "--pipe $PipeName --allow-sid $launcherSid --fresh `"$Template`" `"$Root\runs`" run.cmd"
Step "register task ${TaskName}: `"$Shim`" $shimArgs, as ${AccountName}; ${LauncherUser} may run it" {
    if (-not $task) {
        $action = New-ScheduledTaskAction -Execute $Shim -Argument $shimArgs
        $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Hours 7) `
            -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
        $def = New-ScheduledTask -Action $action -Settings $settings
        Register-ScheduledTask -TaskName $TaskName -InputObject $def -User "$env:COMPUTERNAME\$AccountName" `
            -Password $plain | Out-Null
    }
    $svc = New-Object -ComObject Schedule.Service
    $svc.Connect()
    $t = $svc.GetFolder('\').GetTask($TaskName)
    $t.SetSecurityDescriptor("D:(A;;FA;;;BA)(A;;FA;;;SY)(A;;GRGX;;;$launcherSid)", 0)
}
$plain = $null

if (-not $Apply) {
    Write-Host "`nDry run: nothing was changed. Re-run with -Apply in an elevated shell."
    Write-Host "Not done by this script either way: firewall limit, wipe of the account's temp/profile folders, starting the task (see .DESCRIPTION)."
    exit 0
}

# --- read the state back (a changed exit code is not a changed state) ------------------------------
Write-Host ''
Write-Host 'State read back:'
Check "user $AccountName exists and is enabled" { (Get-LocalUser -Name $AccountName).Enabled }
Check "user $AccountName is in no group beyond Users" {
    $null = Get-LocalUser -Name $AccountName
    $g = Get-LocalGroup | Where-Object { (Get-LocalGroupMember $_ -ErrorAction SilentlyContinue).Name -contains "$env:COMPUTERNAME\$AccountName" }
    -not ($g | Where-Object Name -ne 'Users')
}
Check "$AccountName holds SeBatchLogonRight" { @(Get-BatchRightSids) -contains "*$sid" }
Check "$Root grants only SYSTEM, Administrators and $AccountName (nobody else, not $LauncherUser)" {
    $ids = (Get-Acl $Root).Access.IdentityReference.Value | Sort-Object -Unique
    $extra = $ids | Where-Object { $_ -notmatch "^(NT AUTHORITY\\SYSTEM|BUILTIN\\Administrators)$" -and $_ -notmatch "\\$AccountName$" }
    -not $extra
}
Check "$Root has no ACE for any other ghr-* account (one runner = one root)" {
    -not ((Get-Acl $Root).Access.IdentityReference.Value | Where-Object { $_ -match '\\ghr-' -and $_ -notmatch "\\$AccountName$" })
}
Check "the pinned runner template is in $Template" { Test-Path $RunCmd }
Check "$AccountName can read but not write the template (a job cannot poison later copies)" {
    $acl = (Get-Acl $Template).Access | Where-Object { $_.IdentityReference.Value -match "\\$AccountName$" }
    ($acl | Where-Object { $_.FileSystemRights -match 'ReadAndExecute' }) -and -not ($acl | Where-Object { $_.FileSystemRights -match 'Modify|FullControl|Write' })
}
Check "$Root\runs exists and holds no leftover copy" { (Test-Path "$Root\runs") -and -not (Get-ChildItem "$Root\runs" -Force) }
Check "task $TaskName runs as $AccountName" { (Get-ScheduledTask -TaskName $TaskName).Principal.UserId -match "$AccountName$" }
Check "task action is the shim with --pipe $PipeName --allow-sid $launcherSid" {
    $a = (Get-ScheduledTask -TaskName $TaskName).Actions[0]
    ($a.Execute -eq $Shim) -and ($a.Arguments -eq $shimArgs)
}
Check "task allows $LauncherUser to run it and nothing beyond read+run" {
    $sd = (New-Object -ComObject Schedule.Service | ForEach-Object { $_.Connect(); $_.GetFolder('\').GetTask($TaskName) }).GetSecurityDescriptor(4)
    $bad = @(Get-TaskSdProblems -Sddl $sd -LauncherSid $launcherSid -RunAsSid $sid)
    foreach ($b in $bad) { Write-Host "      $b" }
    $bad.Count -eq 0
}
Check "the account can execute $Shim (read+execute granted, none denied)" {
    $acl = (Get-Acl $Shim).Access
    $allow = $acl | Where-Object { $_.AccessControlType -eq 'Allow' -and $_.FileSystemRights -match 'ReadAndExecute|FullControl|Modify' -and $_.IdentityReference.Value -match "(^|\\)($AccountName|Users|Authenticated Users|Everyone)$" }
    $deny = $acl | Where-Object { $_.AccessControlType -eq 'Deny' -and $_.IdentityReference.Value -match "$AccountName$" }
    $allow -and -not $deny
}

if ($results | Where-Object { -not $_.Ok }) { Write-Host "`nFAILED: see the FAIL lines above."; exit 1 }
Write-Host "`nAll properties read back as expected. Next: the negative controls (docs/ACCOUNT-DISPATCH.md), then the 3-run proof."
} catch {
    Write-Host ''
    Write-Host "FAILED: $($_.Exception.Message)"
    Write-Host $_.ScriptStackTrace
    Write-Host 'Nothing after the failing step ran. Read the state back before re-running (the script is safe to re-run).'
    exit 1
}
