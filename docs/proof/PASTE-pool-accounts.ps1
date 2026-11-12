# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  One-time, ELEVATED setup of the account pool (slice A3 P4): N restricted accounts, each with its own
  directory tree, its own scheduled task and its own outbound firewall block. CireSnave's paste.
  See docs/ACCOUNT-POOL-SETUP.md for the design, what it changes and what it does not prove.

.DESCRIPTION
  DRY-RUN BY DEFAULT: without -Apply it changes nothing and prints what it would do. With -Apply it does
  each step and READS THE STATE BACK, printing PASS/FAIL per property; the exit code is non-zero if any
  failed. Safe to re-run (a re-run gives each account a new random password and re-registers its task).
  -Undo removes everything this script made (and only that), reading each removal back.

  It changes NO existing ACL: not on C:\Program Files\antspawn, not on %ProgramData%\antspawn, not on the
  existing antspawn-run account. Every directory it creates is new, and every ACE it adds is for the
  account it belongs to, plus the one optional deny (-DenyPath) which is that account's own ACE.

  Hash-checked like PASTE-redeploy-runner.ps1: it refuses unless the deployed spawn-runner.exe has the
  SHA-256 you pass, so the runner that will be run as these accounts is the build you meant.

  Each account's password is random, used once to register its task, and discarded. Nobody holds it.

.PARAMETER Count         How many accounts (1-8). Names antspawn-pool1..N, tasks AntSpawnPool1..N.
.PARAMETER RunnerSha256  SHA-256 the deployed spawn-runner.exe must have (from the CI artifact's .sha256 file).
.PARAMETER BinDir        Folder holding spawn-runner.exe. Default 'C:\Program Files\antspawn'. Not under C:\Users.
.PARAMETER LauncherUser  The user allowed to spool jobs and run the tasks. Default: the current user.
.PARAMETER DenyPath      Paths each account must not read. Default C:\Projects. The ACE propagates to every file
                 under the path (minutes on a large tree) and names only that account. Pass @() to skip.
.PARAMETER PoolRoot      Default %ProgramData%\antspawn-pool.
.PARAMETER Apply         Make the changes. Needs an elevated PowerShell.
.PARAMETER Undo          Remove what this script made. Needs -Apply too (the same dry-run default).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateRange(1, 8)][int]$Count,
    [Parameter(Mandatory)][string]$RunnerSha256,
    [string]$BinDir = 'C:\Program Files\antspawn',
    [string]$LauncherUser = "$env:USERDOMAIN\$env:USERNAME",
    [string[]]$DenyPath = @('C:\Projects'),
    [string]$PoolRoot = (Join-Path $env:ProgramData 'antspawn-pool'),
    [switch]$Apply,
    [switch]$Undo
)
$ErrorActionPreference = 'Stop'

$Prefix = 'antspawn-pool'          # account names: antspawn-pool1 ...
$TaskPrefix = 'AntSpawnPool'       # task names: AntSpawnPool1 ...
$BinDir = [IO.Path]::GetFullPath($BinDir)
$Exe = Join-Path $BinDir 'spawn-runner.exe'
$PoolRoot = [IO.Path]::GetFullPath($PoolRoot)
$here = $PSScriptRoot
. (Join-Path $here '..\..\scripts\AntSpawnTaskSd.ps1')   # Get-TaskSdProblems

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if ($Apply -and -not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this in an elevated PowerShell for -Apply (dry-run needs no elevation).'
}
if ($BinDir.StartsWith((Join-Path $env:SystemDrive 'Users'), [StringComparison]::OrdinalIgnoreCase)) {
    throw "BinDir $BinDir is under C:\Users: the restricted accounts cannot execute files in a user profile."
}
$launcherSid = ([Security.Principal.NTAccount]$LauncherUser).Translate([Security.Principal.SecurityIdentifier]).Value

$results = [System.Collections.Generic.List[object]]::new()
function Step([string]$What, [scriptblock]$Do) {
    if ($Apply) { Write-Host "APPLY  $What"; & $Do } else { Write-Host "WOULD  $What" }
}
function Check([string]$Property, [scriptblock]$Test) {
    $ok = $false
    try { $ok = [bool](& $Test) } catch { $ok = $false }
    $results.Add([pscustomobject]@{ Property = $Property; Ok = $ok })
    Write-Host ("{0,-5} {1}" -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $Property)
}
function Acl([string]$Path, [string[]]$IcaclsArgs) {
    & icacls.exe $Path @IcaclsArgs | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls $Path $IcaclsArgs failed ($LASTEXITCODE)" }
}
function Task-Sd([string]$TaskName) {
    $s = New-Object -ComObject Schedule.Service
    $s.Connect()
    $s.GetFolder('\').GetTask($TaskName)
}

# 'Log on as a batch job' lives in the local security policy; read it as SIDs.
function Get-BatchRightSids {
    $f = Join-Path ([IO.Path]::GetTempPath()) ('antspawn-sec-' + [guid]::NewGuid().ToString('N') + '.inf')
    try {
        & secedit.exe /export /cfg $f /areas USER_RIGHTS | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "secedit /export failed ($LASTEXITCODE)" }
        $line = Get-Content $f | Where-Object { $_ -match '^SeBatchLogonRight\s*=' }
        if ($line) {
            ($line -replace '^[^=]*=\s*', '').Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ } | ForEach-Object {
                if ($_.StartsWith('*')) { $_ } else { '*' + ([Security.Principal.NTAccount]$_).Translate([Security.Principal.SecurityIdentifier]).Value }
            }
        } else { @() }
    } finally { Remove-Item $f -ErrorAction SilentlyContinue }
}
function Set-BatchRightSids([string[]]$Sids) {
    $inf = Join-Path ([IO.Path]::GetTempPath()) ('antspawn-sec-' + [guid]::NewGuid().ToString('N') + '.inf')
    $db = [IO.Path]::ChangeExtension($inf, '.sdb')
    try {
        Set-Content -Path $inf -Encoding unicode -Value "[Unicode]`r`nUnicode=yes`r`n[Version]`r`nsignature=`"`$CHICAGO`$`"`r`nRevision=1`r`n[Privilege Rights]`r`nSeBatchLogonRight = $($Sids -join ',')`r`n"
        & secedit.exe /configure /db $db /cfg $inf /areas USER_RIGHTS | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "secedit /configure failed ($LASTEXITCODE)" }
    } finally { Remove-Item $inf, $db -ErrorAction SilentlyContinue }
}

# The pool accounts that exist now (undo works on what is there, whatever -Count says).
function Existing-PoolUsers { @(Get-LocalUser | Where-Object { $_.Name -like "$Prefix*" }) }

# ----------------------------------------------------------------------------------------------------
if ($Undo) {
    Write-Host "UNDO: remove every $Prefix* account, its task, firewall rule, directories, batch-logon right and deny ACEs."
    $users = Existing-PoolUsers
    if (-not $users) { Write-Host "No $Prefix* account exists; checking for leftovers only." }
    foreach ($u in $users) {
        $name = $u.Name; $sid = $u.SID.Value
        $i = $name.Substring($Prefix.Length); $task = "$TaskPrefix$i"; $fw = "$name`: block all outbound"
        Step "stop and unregister task $task" {
            Stop-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue
            if (Get-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue) { Unregister-ScheduledTask -TaskName $task -Confirm:$false }
        }
        Step "remove firewall rule '$fw'" { Get-NetFirewallRule -DisplayName $fw -ErrorAction SilentlyContinue | Remove-NetFirewallRule }
        foreach ($p in $DenyPath) {
            Step "remove ${name}'s deny ACE from ${p} (the ACE is removed from every file under it: minutes on a large tree)" {
                & icacls.exe $p /remove:d "*$sid" /T /C | Out-Null
            }
        }
        Step "remove $name from 'Log on as a batch job'" {
            $keep = @(Get-BatchRightSids | Where-Object { $_ -ne "*$sid" })
            Set-BatchRightSids $keep
        }
        Step "delete the account $name and its profile, if one was created" {
            Remove-LocalUser -Name $name
            Get-CimInstance Win32_UserProfile -Filter "SID='$sid'" -ErrorAction SilentlyContinue | Remove-CimInstance -ErrorAction SilentlyContinue
        }
        if ($Apply) {
            Check "$name is gone" { -not (Get-LocalUser -Name $name -ErrorAction SilentlyContinue) }
            Check "task $task is gone" { -not (Get-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue) }
            Check "firewall rule '$fw' is gone" { -not (Get-NetFirewallRule -DisplayName $fw -ErrorAction SilentlyContinue) }
            Check "$name no longer holds SeBatchLogonRight" { @(Get-BatchRightSids) -notcontains "*$sid" }
            foreach ($p in $DenyPath) {
                Check "no ACE for $name remains on $p" {
                    -not ((Get-Acl $p).Access | Where-Object { $_.IdentityReference.Value -match "$name$" -or $_.IdentityReference.Value -eq $sid })
                }
            }
        }
    }
    Step "delete $PoolRoot (only this script's tree; the existing %ProgramData%\antspawn is not touched)" {
        if (Test-Path $PoolRoot) { Remove-Item -Recurse -Force $PoolRoot }
    }
    if ($Apply) { Check "$PoolRoot is gone" { -not (Test-Path $PoolRoot) } }
    if (-not $Apply) { Write-Host "`nDry run: nothing was changed. Re-run with -Apply in an elevated shell."; exit 0 }
    if ($results | Where-Object { -not $_.Ok }) { Write-Host "`nFAILED: see the FAIL lines above."; exit 1 }
    Write-Host "`nUndone, and each removal read back."; exit 0
}

# ----------------------------------------------------------------------------------------------------
# Preconditions: the runner on disk is the build you meant.
if (-not (Test-Path $Exe)) { throw "spawn-runner.exe not found at $Exe" }
$want = $RunnerSha256.Trim().ToUpper()
$got = (Get-FileHash $Exe -Algorithm SHA256).Hash
if ($got -ne $want) { throw "$Exe has sha256 $got, not $want; refusing. Deploy the build first (docs/REDEPLOY-RUNNER.md)." }
Write-Host "runner hash ok: $got"
$have = Existing-PoolUsers
if ($have.Count -gt $Count) {
    throw "$($have.Count) pool accounts already exist, more than -Count $Count. Run -Undo -Apply first; this script never deletes accounts while setting up."
}

Step "create $PoolRoot (no inheritance; admins and SYSTEM full; $LauncherUser read and traverse; leases\ writable by $LauncherUser only)" {
    New-Item -ItemType Directory -Force $PoolRoot, "$PoolRoot\leases" | Out-Null
    Acl $PoolRoot @('/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', "${LauncherUser}:(RX)")
    Acl "$PoolRoot\leases" @('/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', "${LauncherUser}:(OI)(CI)M")
}

for ($n = 1; $n -le $Count; $n++) {
    $name = "$Prefix$n"; $task = "$TaskPrefix$n"; $dir = Join-Path $PoolRoot $name
    $fw = "$name`: block all outbound"
    Write-Host "`n--- $name / $task ---"
    $plain = $null
    if ($Apply) {
        do {
            $bytes = [byte[]]::new(48)
            $rng = [Security.Cryptography.RandomNumberGenerator]::Create(); $rng.GetBytes($bytes); $rng.Dispose()
            $plain = [Convert]::ToBase64String($bytes)
        } until (($plain -cmatch '[A-Z]') -and ($plain -cmatch '[a-z]') -and ($plain -match '[0-9]'))
    }
    Step "create local user $name (random password, never expires, cannot change it)" {
        $pw = ConvertTo-SecureString $plain -AsPlainText -Force
        if (Get-LocalUser -Name $name -ErrorAction SilentlyContinue) { Set-LocalUser -Name $name -Password $pw }
        else {
            New-LocalUser -Name $name -Password $pw -PasswordNeverExpires -UserMayNotChangePassword -AccountNeverExpires `
                -Description 'AntSpawn pool runner. Nobody holds this password.' | Out-Null
        }
    }
    $account = Get-LocalUser -Name $name -ErrorAction SilentlyContinue
    $sid = if ($account) { $account.SID.Value } else { 'S-1-0-0' }   # dry run: placeholder, nothing uses it

    Step "create $dir with spool, results, work: only this account's own ACEs" {
        New-Item -ItemType Directory -Force $dir, "$dir\spool", "$dir\results", "$dir\work" | Out-Null
        Acl $dir @('/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', "*${sid}:(OI)(CI)RX", "${LauncherUser}:(OI)(CI)RX")
        Acl "$dir\spool" @('/grant:r', "${LauncherUser}:(OI)(CI)M", "*${sid}:(OI)(CI)RX")
        Acl "$dir\results" @('/grant:r', "*${sid}:(OI)(CI)M", "${LauncherUser}:(OI)(CI)(RX,D,DC)")
        Acl "$dir\work" @('/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', "*${sid}:(OI)(CI)M")
        Set-Content -Path "$dir\runner.toml" -Value "max_time_s = 600`nmax_ram_mb = 4096`n" -Encoding ascii
    }

    foreach ($p in $DenyPath) {
        Step "deny $name all access to $p (the ACE names only $name; it propagates to everything under ${p}: minutes on a large tree)" {
            Acl $p @('/deny', "*${sid}:(OI)(CI)F")
        }
    }

    Step "grant $name 'Log on as a batch job' (SeBatchLogonRight)" {
        $cur = @(Get-BatchRightSids)
        if ($cur -notcontains "*$sid") { Set-BatchRightSids (@($cur) + "*$sid") }
    }

    Step "firewall: block all outbound for $name ('$fw')" {
        if (-not (Get-NetFirewallRule -DisplayName $fw -ErrorAction SilentlyContinue)) {
            New-NetFirewallRule -DisplayName $fw -Direction Outbound -Action Block -LocalUser "D:(A;;CC;;;$sid)" -Profile Any | Out-Null
        }
    }

    Step "register task $task (spawn-runner.exe --root `"$dir`" serve, as $name); $LauncherUser may run it, nothing else" {
        $action = New-ScheduledTaskAction -Execute $Exe -Argument "--root `"$dir`" serve"
        $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Hours 2) `
            -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
        Register-ScheduledTask -TaskName $task -InputObject (New-ScheduledTask -Action $action -Settings $settings) `
            -User "$env:COMPUTERNAME\$name" -Password $plain -Force | Out-Null
        (Task-Sd $task).SetSecurityDescriptor("D:(A;;FA;;;BA)(A;;FA;;;SY)(A;;GRGX;;;$launcherSid)", 0)
    }
    $plain = $null

    $t0 = [datetime]::MinValue
    Step "start $task once (empty spool: should exit 0) and read LastTaskResult" {
        $now = Get-Date
        $script:t0 = $now.AddTicks(-($now.Ticks % [TimeSpan]::TicksPerSecond))
        Start-ScheduledTask -TaskName $task
        $deadline = (Get-Date).AddSeconds(60)
        do {
            Start-Sleep -Milliseconds 500
            $i = Get-ScheduledTaskInfo -TaskName $task
            $done = ($i.LastRunTime -ge $script:t0) -and ((Get-ScheduledTask -TaskName $task).State -eq 'Ready')
        } until ($done -or (Get-Date) -gt $deadline)
    }

    if ($Apply) {
        Check "$name exists, is enabled, and is in no group beyond Users" {
            if (-not (Get-LocalUser -Name $name).Enabled) { return $false }
            $g = Get-LocalGroup | Where-Object { (Get-LocalGroupMember $_ -ErrorAction SilentlyContinue).Name -contains "$env:COMPUTERNAME\$name" }
            -not ($g | Where-Object Name -ne 'Users')
        }
        Check "$name holds SeBatchLogonRight" { @(Get-BatchRightSids) -contains "*$sid" }
        Check "task $task runs as $name" { (Get-ScheduledTask -TaskName $task).Principal.UserId -match "$name$" }
        Check "task $task action is `"$Exe`" --root `"$dir`" serve" {
            $a = (Get-ScheduledTask -TaskName $task).Actions[0]
            ($a.Execute -eq $Exe) -and ($a.Arguments -eq "--root `"$dir`" serve")
        }
        Check "task $task descriptor lets only $LauncherUser run it" {
            $bad = @(Get-TaskSdProblems -Sddl (Task-Sd $task).GetSecurityDescriptor(4) -LauncherSid $launcherSid -RunAsSid $sid)
            foreach ($b in $bad) { Write-Host "      $b" }
            $bad.Count -eq 0
        }
        Check "task $task started as $name and exited 0 (LastTaskResult)" {
            $i = Get-ScheduledTaskInfo -TaskName $task
            if ($i.LastTaskResult -ne 0) { Write-Host ("      LastTaskResult = 0x{0:X}" -f $i.LastTaskResult) }
            ($i.LastRunTime -ge $t0) -and ($i.LastTaskResult -eq 0)
        }
        Check "firewall blocks outbound for $name" { Get-NetFirewallRule -DisplayName $fw -ErrorAction Stop | Where-Object { $_.Action -eq 'Block' } }
        Check "$name's tree: it writes work and results, not spool; the launcher writes spool, not work" {
            $w = { param($p, $who) (Get-Acl $p).Access | Where-Object { $_.AccessControlType -eq 'Allow' -and $_.FileSystemRights -match 'Modify|FullControl|Write' -and $_.IdentityReference.Value -match $who } }
            $acct = "(^|\\)$name$"; $lau = [regex]::Escape($LauncherUser)
            (& $w "$dir\work" $acct) -and (& $w "$dir\results" $acct) -and -not (& $w "$dir\spool" $acct) `
                -and (& $w "$dir\spool" $lau) -and -not (& $w "$dir\work" $lau)
        }
        foreach ($p in $DenyPath) {
            Check "$name is denied on $p" { (Get-Acl $p).Access | Where-Object { $_.AccessControlType -eq 'Deny' -and $_.IdentityReference.Value -match "$name$" } }
        }
    }
}

if ($Apply) {
    Check "no account's tree is readable by another pool account" {
        $names = 1..$Count | ForEach-Object { "$Prefix$_" }
        $bad = foreach ($a in $names) { foreach ($b in $names) { if ($a -ne $b) {
            (Get-Acl (Join-Path $PoolRoot $a)).Access | Where-Object { $_.IdentityReference.Value -match "$b$" }
        } } }
        -not $bad
    }
}

if (-not $Apply) { Write-Host "`nDry run: nothing was changed. Re-run with -Apply in an elevated shell."; exit 0 }
if ($results | Where-Object { -not $_.Ok }) { Write-Host "`nFAILED: see the FAIL lines above. -Undo -Apply removes everything."; exit 1 }
Write-Host "`nAll properties read back as expected. This proves setup, not isolation: now run the pool probes (docs/ACCOUNT-POOL-SETUP.md)."
