# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Slice V7 of docs/SANDBOX-BROKER.md: the VM broker's own dedicated, minimally privileged account.
  One-time, ELEVATED, CireSnave's paste. UNRUN: nothing here has been applied on any machine.

.DESCRIPTION
  DRY-RUN BY DEFAULT: without -Apply it changes nothing and prints what it would do. With -Apply it does
  each step and READS THE STATE BACK, printing PASS/FAIL per property; the exit code is non-zero if any
  failed. -Undo -Apply removes what this script made (and only that), reading each removal back.

  Makes, all new:
    * local user `antspawn-broker`: random password used once to register the task and discarded (nobody
      holds it); member of `Hyper-V Administrators` and nothing else but `Users`; NOT an administrator.
    * rights: `Log on as a batch job` granted; interactive logon and Remote Desktop logon DENIED (it never
      has a desktop; whether a Windows guest needs one is V6's question, not decided here).
    * %ProgramData%\antspawn-broker, no inheritance, with
        config\  the broker's configuration (broker.toml): the broker reads it, only administrators write it
        base\    the base images, hash-pinned in broker.toml: the broker reads, only administrators write
        vms\     the VMs' differencing disks and seed ISOs: the broker writes
      Every ACE names the account, the launcher user, administrators or SYSTEM.
    * a deny for the account on each -DenyPath (default C:\Projects): its own ACE, nothing else.
    * task `AntSpawnBroker`: `spawn-broker.exe --config <root>\config\broker.toml` as the account, no time
      limit, one instance; only administrators, SYSTEM and the launcher user may run it.
  It does NOT start the task and does NOT write broker.toml: the file holds the runner accounts' SIDs and
  the image hashes, which are yours (see docs/SANDBOX-BROKER.md and src/broker_config.rs). It adds NO
  firewall rule (a rule would confound "does the limited account hamper the VMs"; add one after V7's table).
  It touches no existing account, no existing ACL, no Hyper-V switch, no VM, no WSL.

  -Undo removes: the task, the account's deny ACEs, its three rights entries, the account and its profile,
  then the folder. It REFUSES to delete the folder while any `antspawn-*` VM exists (deleting the disks
  under a live VM corrupts it): stop the broker, let its next start or `Remove-VM` clear them, run -Undo again.

  Hash-checked like PASTE-pool-accounts.ps1: it refuses unless spawn-broker.exe has the SHA-256 you pass.

.PARAMETER BrokerSha256  SHA-256 spawn-broker.exe must have (from the CI artifact's .sha256 file).
.PARAMETER BinDir        Folder holding spawn-broker.exe. Default 'C:\Program Files\antspawn'. Not under C:\Users.
.PARAMETER LauncherUser  The user allowed to run the task. Default: the current user.
.PARAMETER DenyPath      Paths the account must not read. Default C:\Projects (minutes on a large tree). Pass @() to skip.
.PARAMETER Root          Default %ProgramData%\antspawn-broker.
.PARAMETER Apply         Make the changes. Needs an elevated PowerShell.
.PARAMETER Undo          Remove what this script made. Needs -Apply too (the same dry-run default).
#>
[CmdletBinding()]
param(
    [string]$BrokerSha256,   # required unless -Undo
    [string]$BinDir = 'C:\Program Files\antspawn',
    [string]$LauncherUser = "$env:USERDOMAIN\$env:USERNAME",
    [string[]]$DenyPath = @('C:\Projects'),
    [string]$Root = (Join-Path $env:ProgramData 'antspawn-broker'),
    [switch]$Apply,
    [switch]$Undo
)
$ErrorActionPreference = 'Stop'

$Name = 'antspawn-broker'
$Task = 'AntSpawnBroker'
$HyperVAdminsSid = 'S-1-5-32-578'
$BinDir = [IO.Path]::GetFullPath($BinDir)
$Exe = Join-Path $BinDir 'spawn-broker.exe'
$Root = [IO.Path]::GetFullPath($Root)
$Config = Join-Path $Root 'config\broker.toml'
. (Join-Path $PSScriptRoot 'AntSpawnTaskSd.ps1')   # Get-TaskSdProblems

# Every string handed to the account cmdlets, against the limits those cmdlets enforce, BEFORE anything is
# changed and in the dry run too (a dry run never calls New-LocalUser; the pool paste's first real -Apply
# stopped on a 49-character -Description). Limits: SAM account name 20, -Description 48, local password 127,
# task name 238, task -User string 104.
$BrokerDescription = 'AntSpawn VM broker. Nobody holds the password.'
$passwordLength = [Math]::Ceiling(48 / 3) * 4   # the password is base64 of 48 bytes (see below)
$limitChecks = @(
    @('account name', $Name, 20),
    @('account -Description', $BrokerDescription, 48),
    @('generated password', ('x' * $passwordLength), 127),
    @('task name', $Task, 238),
    @('task -User string', "$env:COMPUTERNAME\$Name", 104)
)
foreach ($lc in $limitChecks) {
    if ($lc[1].Length -gt $lc[2]) { throw "$($lc[0]) is $($lc[1].Length) characters; the cmdlet allows $($lc[2]). Nothing was changed." }
}
if ($Name -match '["/\\[\]:;|=,+*?<>]') { throw "$Name contains a character a local user name cannot have. Nothing was changed." }

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if ($Apply -and -not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this in an elevated PowerShell for -Apply (dry-run needs no elevation).'
}
if ($BinDir.StartsWith((Join-Path $env:SystemDrive 'Users'), [StringComparison]::OrdinalIgnoreCase)) {
    throw "BinDir $BinDir is under C:\Users: the broker account cannot execute files in a user profile."
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

# User rights live in the local security policy; read and write one right as SIDs.
function Get-RightSids([string]$Right) {
    $f = Join-Path ([IO.Path]::GetTempPath()) ('antspawn-sec-' + [guid]::NewGuid().ToString('N') + '.inf')
    try {
        & secedit.exe /export /cfg $f /areas USER_RIGHTS | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "secedit /export failed ($LASTEXITCODE)" }
        $line = Get-Content $f | Where-Object { $_ -match "^$Right\s*=" }
        if ($line) {
            ($line -replace '^[^=]*=\s*', '').Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ } | ForEach-Object {
                if ($_.StartsWith('*')) { $_ } else { '*' + ([Security.Principal.NTAccount]$_).Translate([Security.Principal.SecurityIdentifier]).Value }
            }
        } else { @() }
    } finally { Remove-Item $f -ErrorAction SilentlyContinue }
}
function Set-RightSids([string]$Right, [string[]]$Sids) {
    $inf = Join-Path ([IO.Path]::GetTempPath()) ('antspawn-sec-' + [guid]::NewGuid().ToString('N') + '.inf')
    $db = [IO.Path]::ChangeExtension($inf, '.sdb')
    try {
        Set-Content -Path $inf -Encoding unicode -Value "[Unicode]`r`nUnicode=yes`r`n[Version]`r`nsignature=`"`$CHICAGO`$`"`r`nRevision=1`r`n[Privilege Rights]`r`n$Right = $($Sids -join ',')`r`n"
        & secedit.exe /configure /db $db /cfg $inf /areas USER_RIGHTS | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "secedit /configure failed ($LASTEXITCODE)" }
    } finally { Remove-Item $inf, $db -ErrorAction SilentlyContinue }
}
function Add-Right([string]$Right, [string]$Sid) {
    $cur = @(Get-RightSids $Right)
    if ($cur -notcontains "*$Sid") { Set-RightSids $Right (@($cur) + "*$Sid") }
}
function Remove-Right([string]$Right, [string]$Sid) {
    Set-RightSids $Right @(Get-RightSids $Right | Where-Object { $_ -ne "*$Sid" })
}
$Rights = @('SeBatchLogonRight', 'SeDenyInteractiveLogonRight', 'SeDenyRemoteInteractiveLogonRight')

# ----------------------------------------------------------------------------------------------------
if ($Undo) {
    Write-Host "UNDO: remove $Name, its task, rights, deny ACEs and $Root."
    $acct = Get-LocalUser -Name $Name -ErrorAction SilentlyContinue
    $sid = if ($acct) { $acct.SID.Value } else { $null }
    Step "stop and unregister task $Task" {
        Stop-ScheduledTask -TaskName $Task -ErrorAction SilentlyContinue
        if (Get-ScheduledTask -TaskName $Task -ErrorAction SilentlyContinue) { Unregister-ScheduledTask -TaskName $Task -Confirm:$false }
    }
    if ($sid) {
        foreach ($p in $DenyPath) {
            Step "remove ${Name}'s deny ACE from ${p} (minutes on a large tree)" { & icacls.exe $p /remove:d "*$sid" /T /C | Out-Null }
        }
        foreach ($r in $Rights) { Step "remove $Name from $r" { Remove-Right $r $sid } }
        Step "delete the account $Name (its Hyper-V Administrators membership goes with it) and its profile, if one was created" {
            Remove-LocalUser -Name $Name
            Get-CimInstance Win32_UserProfile -Filter "SID='$sid'" -ErrorAction SilentlyContinue | Remove-CimInstance -ErrorAction SilentlyContinue
        }
    } else { Write-Host "No $Name account exists; checking for leftovers only." }
    # Never delete disks under a live VM.
    $vms = @()
    if (Get-Command Get-VM -ErrorAction SilentlyContinue) { $vms = @(Get-VM -Name 'antspawn-*' -ErrorAction SilentlyContinue) }
    if ($vms.Count -gt 0) {
        Write-Host "KEEP   $Root : $($vms.Count) antspawn-* VM(s) exist ($(($vms | ForEach-Object Name) -join ', ')); their disks may be under it."
        Write-Host "       Remove them (the broker's next start sweeps them, or Remove-VM), then run -Undo -Apply again."
    } else {
        Step "delete $Root (only this script's tree)" { if (Test-Path $Root) { Remove-Item -Recurse -Force $Root } }
    }
    if (-not $Apply) { Write-Host "`nDry run: nothing was changed. Re-run with -Apply in an elevated shell."; exit 0 }
    if ($sid) {
        Check "$Name is gone" { -not (Get-LocalUser -Name $Name -ErrorAction SilentlyContinue) }
        foreach ($r in $Rights) { Check "$Name no longer holds $r" { @(Get-RightSids $r) -notcontains "*$sid" } }
        foreach ($p in $DenyPath) {
            Check "no ACE for $Name remains on $p" {
                -not ((Get-Acl $p).Access | Where-Object { $_.IdentityReference.Value -match "$Name$" -or $_.IdentityReference.Value -eq $sid })
            }
        }
    }
    Check "task $Task is gone" { -not (Get-ScheduledTask -TaskName $Task -ErrorAction SilentlyContinue) }
    if ($vms.Count -eq 0) { Check "$Root is gone" { -not (Test-Path $Root) } }
    if ($results | Where-Object { -not $_.Ok }) { Write-Host "`nFAILED: see the FAIL lines above."; exit 1 }
    Write-Host "`nUndone, and each removal read back."; exit 0
}

# ----------------------------------------------------------------------------------------------------
# Preconditions: the broker on disk is the build you meant.
if (-not $BrokerSha256) { throw '-BrokerSha256 is required (the SHA-256 spawn-broker.exe must have).' }
if (-not (Test-Path $Exe)) { throw "spawn-broker.exe not found at $Exe" }
$want = $BrokerSha256.Trim().ToUpper()
$got = (Get-FileHash $Exe -Algorithm SHA256).Hash
if ($got -ne $want) { throw "$Exe has sha256 $got, not $want; refusing." }
Write-Host "broker hash ok: $got"
if (-not (Get-LocalGroup -SID $HyperVAdminsSid -ErrorAction SilentlyContinue)) {
    throw "The 'Hyper-V Administrators' group ($HyperVAdminsSid) does not exist: is the Hyper-V feature installed?"
}

$plain = $null
if ($Apply) {
    do {
        $bytes = [byte[]]::new(48)
        $rng = [Security.Cryptography.RandomNumberGenerator]::Create(); $rng.GetBytes($bytes); $rng.Dispose()
        $plain = [Convert]::ToBase64String($bytes)
    } until (($plain -cmatch '[A-Z]') -and ($plain -cmatch '[a-z]') -and ($plain -match '[0-9]'))
}
# New-LocalUser -Description takes at most 48 characters (the first real setup run failed on 59).
Step "create local user $Name (random password, never expires, cannot change it)" {
    $pw = ConvertTo-SecureString $plain -AsPlainText -Force
    if (Get-LocalUser -Name $Name -ErrorAction SilentlyContinue) { Set-LocalUser -Name $Name -Password $pw }
    else {
        New-LocalUser -Name $Name -Password $pw -PasswordNeverExpires -UserMayNotChangePassword -AccountNeverExpires `
            -Description $BrokerDescription | Out-Null
    }
}
$account = Get-LocalUser -Name $Name -ErrorAction SilentlyContinue
$sid = if ($account) { $account.SID.Value } else { 'S-1-0-0' }   # dry run: placeholder, nothing uses it

Step "add $Name to 'Hyper-V Administrators' ($HyperVAdminsSid): its only group beyond Users" {
    $g = Get-LocalGroup -SID $HyperVAdminsSid
    if (-not (Get-LocalGroupMember -Group $g -ErrorAction SilentlyContinue | Where-Object { $_.SID.Value -eq $sid })) {
        Add-LocalGroupMember -Group $g -Member $Name
    }
}
Step "rights for ${Name}: grant 'Log on as a batch job'; deny interactive and Remote Desktop logon" {
    Add-Right 'SeBatchLogonRight' $sid
    Add-Right 'SeDenyInteractiveLogonRight' $sid
    Add-Right 'SeDenyRemoteInteractiveLogonRight' $sid
}

Step "create ${Root}: no inheritance; admins and SYSTEM full; $LauncherUser and $Name read (config and base), $Name write (vms only)" {
    New-Item -ItemType Directory -Force $Root, "$Root\config", "$Root\base", "$Root\vms" | Out-Null
    Acl $Root @('/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', "*${sid}:(OI)(CI)RX", "${LauncherUser}:(OI)(CI)RX")
    Acl "$Root\vms" @('/grant:r', "*${sid}:(OI)(CI)M")
}
foreach ($p in $DenyPath) {
    Step "deny $Name all access to $p (its own ACE; propagates to everything under ${p}: minutes on a large tree)" {
        Acl $p @('/deny', "*${sid}:(OI)(CI)F")
    }
}

Step "register task $Task (spawn-broker.exe --config `"$Config`", as $Name, no time limit); $LauncherUser may run it, nothing else; NOT started" {
    $action = New-ScheduledTaskAction -Execute $Exe -Argument "--config `"$Config`""
    $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName $Task -InputObject (New-ScheduledTask -Action $action -Settings $settings) `
        -User "$env:COMPUTERNAME\$Name" -Password $plain -Force | Out-Null
    (Task-Sd $Task).SetSecurityDescriptor("D:(A;;FA;;;BA)(A;;FA;;;SY)(A;;GRGX;;;$launcherSid)", 0)
}
$plain = $null

if ($Apply) {
    Check "$Name is enabled, NOT an administrator, and in no group beyond Users and Hyper-V Administrators" {
        if (-not (Get-LocalUser -Name $Name).Enabled) { return $false }
        $mine = @(Get-LocalGroup | Where-Object { (Get-LocalGroupMember $_ -ErrorAction SilentlyContinue).Name -contains "$env:COMPUTERNAME\$Name" })
        $bad = @($mine | Where-Object { $_.Name -ne 'Users' -and $_.SID.Value -ne $HyperVAdminsSid })
        foreach ($b in $bad) { Write-Host "      unexpected group: $($b.Name)" }
        ($bad.Count -eq 0) -and ($mine | Where-Object { $_.SID.Value -eq $HyperVAdminsSid })
    }
    Check "$Name holds SeBatchLogonRight and is denied interactive and Remote Desktop logon" {
        (@(Get-RightSids 'SeBatchLogonRight') -contains "*$sid") -and (@(Get-RightSids 'SeDenyInteractiveLogonRight') -contains "*$sid") `
            -and (@(Get-RightSids 'SeDenyRemoteInteractiveLogonRight') -contains "*$sid")
    }
    Check "task $Task runs as $Name" { (Get-ScheduledTask -TaskName $Task).Principal.UserId -match "$Name$" }
    Check "task $Task action is `"$Exe`" --config `"$Config`"" {
        $a = (Get-ScheduledTask -TaskName $Task).Actions[0]
        ($a.Execute -eq $Exe) -and ($a.Arguments -eq "--config `"$Config`"")
    }
    Check "task $Task descriptor lets only $LauncherUser run it" {
        $bad = @(Get-TaskSdProblems -Sddl (Task-Sd $Task).GetSecurityDescriptor(4) -LauncherSid $launcherSid -RunAsSid $sid)
        foreach ($b in $bad) { Write-Host "      $b" }
        $bad.Count -eq 0
    }
    Check "$Name writes only vms\ (config\ and base\ are read-only to it)" {
        $w = { param($p) (Get-Acl $p).Access | Where-Object { $_.AccessControlType -eq 'Allow' -and $_.FileSystemRights -match 'Modify|FullControl|Write' -and $_.IdentityReference.Value -match "(^|\\)$Name$" } }
        (& $w "$Root\vms") -and -not (& $w "$Root\config") -and -not (& $w "$Root\base") -and -not (& $w $Root)
    }
    foreach ($p in $DenyPath) {
        Check "$Name is denied on $p" { (Get-Acl $p).Access | Where-Object { $_.AccessControlType -eq 'Deny' -and $_.IdentityReference.Value -match "$Name$" } }
    }
}

if (-not $Apply) { Write-Host "`nDry run: nothing was changed. Re-run with -Apply in an elevated shell."; exit 0 }
if ($results | Where-Object { -not $_.Ok }) { Write-Host "`nFAILED: see the FAIL lines above. -Undo -Apply removes everything."; exit 1 }
Write-Host "`nAll properties read back as expected. This proves setup, not that the broker works as this account."
Write-Host "Next (yours, not done here): write $Config (docs/SANDBOX-BROKER.md), run 'spawn-broker.exe --config <it> --check', put base images in $Root\base, then Start-ScheduledTask $Task."
