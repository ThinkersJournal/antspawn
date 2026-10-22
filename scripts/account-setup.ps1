# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  One-time, elevated setup for AntSpawn's `account` launcher. Run by the machine owner, never by a lane.

.DESCRIPTION
  Creates the restricted local account, the scheduled task that runs `spawn-runner serve` AS that
  account, and the spool/results/work directories with their ACLs. The account's password is random,
  is used once to register the task, and is then discarded: nobody holds it, only the service
  manager. DRY-RUN BY DEFAULT: without -Apply it changes nothing and prints what it would do.
  With -Apply it does each step, then READS THE STATE BACK and prints PASS/FAIL per property; the
  exit code is non-zero if any property failed. Safe to re-run.

  It does NOT prove isolation. After -Apply, run the negative tests: `cargo test --test account`.

.PARAMETER Apply         Make the changes (needs an elevated PowerShell).
.PARAMETER BinDir        Folder holding spawn-runner.exe. The account must be able to execute it, so it may NOT be under
                 C:\Users (a standard user cannot read another profile); use e.g. C:\Program Files\antspawn
                 (copy spawn-runner.exe there first, elevated). It must not be writable by the account.
.PARAMETER LauncherUser  The user allowed to spool jobs and trigger the task. Default: the current user.
.PARAMETER DenyPath      Paths the account must not read (ACL deny). Default: C:\Projects.
.PARAMETER MaxTimeS      Longest job time limit the runner accepts.
.PARAMETER MaxRamMb      Largest per-process memory limit the runner accepts.
#>
[CmdletBinding()]
param(
    [switch]$Apply,
    [Parameter(Mandatory)][string]$BinDir,
    [string]$LauncherUser = "$env:USERDOMAIN\$env:USERNAME",
    [string[]]$DenyPath = @('C:\Projects'),
    [int]$MaxTimeS = 600,
    [int]$MaxRamMb = 4096
)
$ErrorActionPreference = 'Stop'

$AccountName = 'antspawn-run'
$TaskName = 'AntSpawnRunner'      # must equal antspawn::runner::TASK_NAME
$Root = Join-Path $env:ProgramData 'antspawn'
$BinDir = [IO.Path]::GetFullPath($BinDir)
$Exe = Join-Path $BinDir 'spawn-runner.exe'
$usersRoot = Join-Path $env:SystemDrive 'Users'
if ($BinDir.StartsWith($usersRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "BinDir $BinDir is under ${usersRoot}: the restricted account cannot execute files in a user profile. Copy spawn-runner.exe to e.g. 'C:\Program Files\antspawn' and pass that as -BinDir."
}

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
$elevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($Apply -and -not $elevated) { throw 'Run this in an elevated PowerShell for -Apply (dry-run needs no elevation).' }
if (-not (Test-Path $Exe)) { throw "spawn-runner.exe not found at $Exe" }

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

# --- the account (password: random, used once below, never stored) ---------------------------------
$account = Get-LocalUser -Name $AccountName -ErrorAction SilentlyContinue
$task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
$needPassword = (-not $account) -or (-not $task)
$plain = $null
if ($Apply -and $needPassword) {
    $bytes = [byte[]]::new(48)
    [Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
    $plain = [Convert]::ToBase64String($bytes)
}
Step "create local user $AccountName (random password, never expires, cannot change it)" {
    if (-not $account) {
        New-LocalUser -Name $AccountName -Password (ConvertTo-SecureString $plain -AsPlainText -Force) `
            -PasswordNeverExpires -UserMayNotChangePassword -AccountNeverExpires `
            -Description 'AntSpawn job runner. Nobody holds this password.' | Out-Null
    } elseif (-not $task) {
        # The task must be re-registered, which needs a password; a new random one replaces the old.
        Set-LocalUser -Name $AccountName -Password (ConvertTo-SecureString $plain -AsPlainText -Force)
    }
}
$account = Get-LocalUser -Name $AccountName -ErrorAction SilentlyContinue
$sid = if ($account) { $account.SID.Value } else { $null }

# --- directories and ACLs --------------------------------------------------------------------------
Step "create $Root with spool, results, work" {
    foreach ($d in @($Root, "$Root\spool", "$Root\results", "$Root\work")) { New-Item -ItemType Directory -Force $d | Out-Null }
}
Step "root: admins+SYSTEM full; account and $LauncherUser read-only; no inheritance from ProgramData" {
    Acl $Root @('/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F',
        "*${sid}:(OI)(CI)RX", "${LauncherUser}:(OI)(CI)RX")
}
Step "spool: only $LauncherUser writes; the account reads" {
    Acl "$Root\spool" @('/grant:r', "${LauncherUser}:(OI)(CI)M", "*${sid}:(OI)(CI)RX")
}
Step "results: the account writes; $LauncherUser reads and deletes its own job dirs" {
    Acl "$Root\results" @('/grant:r', "*${sid}:(OI)(CI)M", "${LauncherUser}:(OI)(CI)(RX,D,DC)")
}
Step "work: the account only" {
    Acl "$Root\work" @('/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', "*${sid}:(OI)(CI)M")
}
Step "runner.toml: max_time_s=$MaxTimeS max_ram_mb=$MaxRamMb (admin-written; the account cannot change it)" {
    Set-Content -Path "$Root\runner.toml" -Value "max_time_s = $MaxTimeS`nmax_ram_mb = $MaxRamMb`n" -Encoding ascii
}
foreach ($p in $DenyPath) {
    Step "deny the account all access to $p (WARNING: the ACE propagates to EVERY file and folder under $p, which can take minutes on a large tree; it affects only $AccountName, no other user or lane)" { Acl $p @('/deny', "*${sid}:(OI)(CI)F") }
}
Step "deny the account write access to $BinDir" { Acl $BinDir @('/deny', "*${sid}:(OI)(CI)(W,D,DC)") }

# --- no network for the account --------------------------------------------------------------------
$fw = "antspawn-run: block all outbound"
Step "firewall: block outbound for the account ($fw)" {
    if (-not (Get-NetFirewallRule -DisplayName $fw -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -DisplayName $fw -Direction Outbound -Action Block `
            -LocalUser "D:(A;;CC;;;$sid)" -Profile Any | Out-Null
    }
}

# --- the task, run AS the account; $LauncherUser may run it but not change it ----------------------
Step "register task ${TaskName} (serve as ${AccountName}); ${LauncherUser} may run it" {
    if (-not $task) {
        $action = New-ScheduledTaskAction -Execute $Exe -Argument 'serve'
        $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Hours 2) `
            -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
        $def = New-ScheduledTask -Action $action -Settings $settings
        Register-ScheduledTask -TaskName $TaskName -InputObject $def -User "$env:COMPUTERNAME\$AccountName" `
            -Password $plain | Out-Null
    }
    $launcherSid = ([Security.Principal.NTAccount]$LauncherUser).Translate([Security.Principal.SecurityIdentifier]).Value
    $svc2 = New-Object -ComObject Schedule.Service
    $svc2.Connect()
    $t = $svc2.GetFolder('\').GetTask($TaskName)
    # Admins and SYSTEM full control; the launcher user may read and run (GR GX) and nothing else.
    $t.SetSecurityDescriptor("D:(A;;FA;;;BA)(A;;FA;;;SY)(A;;GRGX;;;$launcherSid)", 0)
}
$plain = $null

# --- start the task once: registered is not the same as able to log on and run ---------------------
$t0 = [datetime]::MinValue
Step "start task $TaskName once (empty spool, so it should exit 0) and read LastTaskResult; needs the account to be allowed 'Log on as a batch job'" {
    $script:t0 = Get-Date
    Start-ScheduledTask -TaskName $TaskName
    $deadline = (Get-Date).AddSeconds(60)
    do {
        Start-Sleep -Milliseconds 500
        $i = Get-ScheduledTaskInfo -TaskName $TaskName
        $done = ($i.LastRunTime -ge $script:t0) -and ((Get-ScheduledTask -TaskName $TaskName).State -eq 'Ready')
    } until ($done -or (Get-Date) -gt $deadline)
}

# --- read the state back (a changed exit code is not a changed state) ------------------------------
Write-Host ''
Write-Host 'State read back:'
Check "user $AccountName exists and is enabled" { (Get-LocalUser -Name $AccountName).Enabled }
Check "user $AccountName is in no group beyond Users" {
    $null = Get-LocalUser -Name $AccountName  # absent user: throws, so this FAILs rather than passes vacuously
    $g = Get-LocalGroup | Where-Object { (Get-LocalGroupMember $_ -ErrorAction SilentlyContinue).Name -contains "$env:COMPUTERNAME\$AccountName" }
    -not ($g | Where-Object Name -ne 'Users')
}
Check "task $TaskName runs as $AccountName" { (Get-ScheduledTask -TaskName $TaskName).Principal.UserId -match "$AccountName$" }
Check "task action is `"$Exe`" serve" { $a = (Get-ScheduledTask -TaskName $TaskName).Actions[0]; ($a.Execute -eq $Exe) -and ($a.Arguments -eq 'serve') }
Check "task allows $LauncherUser to run it" {
    $sd = (New-Object -ComObject Schedule.Service | ForEach-Object { $_.Connect(); $_.GetFolder('\').GetTask($TaskName) }).GetSecurityDescriptor(4)
    $sd -match 'GRGX'
}
Check "the account can execute $Exe (read+execute granted, none denied)" {
    $acl = (Get-Acl $Exe).Access
    $allow = $acl | Where-Object { $_.AccessControlType -eq 'Allow' -and $_.FileSystemRights -match 'ReadAndExecute|FullControl|Modify' -and $_.IdentityReference.Value -match "(^|\\)($AccountName|Users|Authenticated Users|Everyone)$" }
    $deny = $acl | Where-Object { $_.AccessControlType -eq 'Deny' -and $_.IdentityReference.Value -match "$AccountName$" -and $_.FileSystemRights -match 'ExecuteFile|ReadAndExecute|FullControl' }
    $allow -and -not $deny
}
Check "task $TaskName actually started as $AccountName and exited 0 (LastTaskResult)" {
    $i = Get-ScheduledTaskInfo -TaskName $TaskName
    if ($i.LastTaskResult -ne 0) {
        Write-Host ("      LastTaskResult = 0x{0:X} (0x8007052E / 0x80070569 mean logon refused: grant {1} 'Log on as a batch job' in secpol.msc > Local Policies > User Rights Assignment, then re-run)" -f $i.LastTaskResult, $AccountName)
    }
    ($i.LastRunTime -ge $t0) -and ($i.LastTaskResult -eq 0)
}
Check "$Root\runner.toml has the ceilings" { (Get-Content "$Root\runner.toml" -Raw) -match "max_time_s = $MaxTimeS" }
Check "spool grants $LauncherUser modify and nobody else write" {
    $a = (Get-Acl "$Root\spool").Access
    $w = $a | Where-Object { $_.FileSystemRights -match 'Modify|FullControl|Write' -and $_.AccessControlType -eq 'Allow' }
    ($w.IdentityReference.Value | Sort-Object -Unique | Where-Object { $_ -notmatch 'SYSTEM|Administrators' }) -eq $LauncherUser
}
foreach ($p in $DenyPath) {
    Check "the account is denied on $p" {
        (Get-Acl $p).Access | Where-Object { $_.AccessControlType -eq 'Deny' -and $_.IdentityReference.Value -match "$AccountName$" }
    }
}
Check "firewall blocks outbound for the account" { Get-NetFirewallRule -DisplayName $fw -ErrorAction Stop | Where-Object { $_.Action -eq 'Block' } }

if (-not $Apply) { Write-Host "`nDry run: nothing was changed. Re-run with -Apply in an elevated shell." ; exit 0 }
if ($results | Where-Object { -not $_.Ok }) { Write-Host "`nFAILED: see the FAIL lines above."; exit 1 }
Write-Host "`nAll properties read back as expected. Now run: cargo test --test account"
