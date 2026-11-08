# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Slice V0 of docs/SANDBOX-BROKER.md: MEASURE whether a dedicated non-interactive account in
  Hyper-V Administrators can run a Hyper-V VM lifecycle, and whether a restricted runner can reach
  it. Elevated, run by the machine owner, never by a lane. DRY-RUN BY DEFAULT; -Undo removes everything.

.DESCRIPTION
  -Apply creates, all named `antspawn-v0-*`:
    * local user `antspawn-v0-broker` (random password used once to register two tasks, then discarded),
      added to `Hyper-V Administrators`, granted `Log on as a batch job`. It gets NO other right.
    * folder %ProgramData%\antspawn-v0 (results land in results\; only SYSTEM/Administrators/the broker/you)
    * task `AntSpawnV0Probe`: runs probe.ps1 AS the broker (batch logon, no desktop). It creates an
      Internal switch, a 64 MB parent VHDX and a differencing disk, a generation-2 VM with no OS,
      starts it, power-cuts it (Stop-VM -TurnOff), starts the SAME VM again, power-cuts it, removes
      the VM and disks, and records each step's result and time in results\probe.txt. The switch is
      left for the reachability test. The script runs this task once and waits for it.
    * the host-side address 10.77.0.1/30 on that switch's vEthernet (set HERE, elevated: whether the
      broker could do it is also recorded as a step it attempts first).
    * task `AntSpawnV0Listen` (not started here): as the broker, serves a pipe `antspawn-v0-pipe`
      whose DACL names only the broker and `antspawn-run`, and a TCP listener on 10.77.0.1:2222, for
      15 minutes, logging who connected. YOU (the launcher user) may start it: Start-ScheduledTask.
  Nothing here creates a VM image, touches the existing switches or WSL, adds a firewall rule, or
  changes any other account. The probe VM has no OS and no network adapter beyond the one test switch.

  -Undo removes: both tasks, any antspawn-v0-* VM, switch and disks, the address, the broker's group
  membership and batch right, the user, and the folder. It reads the state back (PASS/FAIL).

  It does NOT prove isolation. It measures facts the design needs (unmeasured items 1, 2, 3 and 4
  and a new switch's forwarding default); the lane runs the client side as antspawn-run afterwards.
#>
[CmdletBinding()]
param(
    [switch]$Apply,
    [switch]$Undo,
    [string]$RunnerUser = 'antspawn-run',
    [string]$LauncherUser = "$env:USERDOMAIN\$env:USERNAME"
)
$ErrorActionPreference = 'Stop'
if ($Apply -and $Undo) { throw 'Choose -Apply or -Undo, not both.' }
$Broker = 'antspawn-v0-broker'
$BrokerDescription = 'AntSpawn V0 broker. Remove: broker-v0.ps1 -Undo'
$Dir = Join-Path $env:ProgramData 'antspawn-v0'
$Res = Join-Path $Dir 'results'
$SwitchName = 'antspawn-v0-sw'
$HostIp = '10.77.0.1'
$Live = $Apply -or $Undo
# Every string handed to the account cmdlets, against the limits those cmdlets enforce. A dry run never
# calls them, so this is what makes the dry run catch an over-long value (New-LocalUser -Description
# is limited to 48, and a 64-character one stopped the first -Apply). Limits: SAM account name 20,
# New-LocalUser -Description 48, local password 127, task name 238 without a backslash.
$passwordLength = ([Math]::Ceiling(48 / 3) * 4) + 3   # base64 of 48 bytes, plus 'aA1' (see below)
$limitChecks = @(
    @('broker user name', $Broker, 20),
    @('broker user -Description', $BrokerDescription, 48),
    @('generated password', ('x' * $passwordLength), 127),
    @('task name AntSpawnV0Probe', 'AntSpawnV0Probe', 238),
    @('task name AntSpawnV0Listen', 'AntSpawnV0Listen', 238),
    @('task -User string', "$env:COMPUTERNAME\$Broker", 104)
)
foreach ($lc in $limitChecks) {
    if ($lc[1].Length -gt $lc[2]) { throw "$($lc[0]) is $($lc[1].Length) characters; the cmdlet allows $($lc[2]). Nothing was changed." }
}
if ($Broker -match '["/\\[\]:;|=,+*?<>]') { throw "$Broker contains a character a local user name cannot have. Nothing was changed." }
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if ($Live -and -not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this in an elevated PowerShell for -Apply or -Undo (the dry run needs none).'
}
$results = [System.Collections.Generic.List[object]]::new()
function Step([string]$What, [scriptblock]$Do) { if ($Live) { Write-Host "DO     $What"; & $Do } else { Write-Host "WOULD  $What" } }
function Check([string]$Property, [scriptblock]$Test) {
    $ok = $false
    try { $ok = [bool](& $Test) } catch { $ok = $false }
    $results.Add([pscustomobject]@{ Property = $Property; Ok = $ok })
}
function Sid([string]$Name) { ([Security.Principal.NTAccount]$Name).Translate([Security.Principal.SecurityIdentifier]).Value }
function Get-BatchRightSids {
    $f = Join-Path ([IO.Path]::GetTempPath()) ("antspawn-sec-" + [guid]::NewGuid().ToString('N') + '.inf')
    try {
        & secedit.exe /export /cfg $f /areas USER_RIGHTS | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "secedit /export failed ($LASTEXITCODE)" }
        $line = Get-Content $f | Where-Object { $_ -match '^SeBatchLogonRight\s*=' }
        if ($line) {
            ($line -replace '^[^=]*=\s*', '').Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ } | ForEach-Object {
                if ($_.StartsWith('*')) { $_ } else { '*' + (Sid $_) }
            }
        } else { @() }
    } finally { Remove-Item $f -ErrorAction SilentlyContinue }
}
function Set-BatchRight([string[]]$Sids) {
    $inf = Join-Path ([IO.Path]::GetTempPath()) ("antspawn-sec-" + [guid]::NewGuid().ToString('N') + '.inf')
    $db = [IO.Path]::ChangeExtension($inf, '.sdb')
    try {
        Set-Content -Path $inf -Encoding unicode -Value "[Unicode]`r`nUnicode=yes`r`n[Version]`r`nsignature=`"`$CHICAGO`$`"`r`nRevision=1`r`n[Privilege Rights]`r`nSeBatchLogonRight = $($Sids -join ',')`r`n"
        & secedit.exe /configure /db $db /cfg $inf /areas USER_RIGHTS | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "secedit /configure failed ($LASTEXITCODE)" }
    } finally { Remove-Item $inf, $db -ErrorAction SilentlyContinue }
}
function Wait-Task([string]$Name, [datetime]$Since, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    do {
        Start-Sleep -Milliseconds 700
        $info = Get-ScheduledTaskInfo -TaskName $Name
        $state = (Get-ScheduledTask -TaskName $Name).State
    } until (($state -ne 'Running' -and $info.LastRunTime -ge $Since) -or (Get-Date) -gt $deadline)
    $info
}

# ---------------------------------------------------------------------------------------------
$probe = @'
# Runs AS antspawn-v0-broker (non-interactive batch logon). Every step is recorded, pass or fail.
$ErrorActionPreference = 'Stop'
$out = 'RESULTS_DIR\probe.txt'
$log = [System.Collections.Generic.List[string]]::new()
function Rec([string]$s) { $log.Add($s); Set-Content -Path $out -Value $log -Encoding utf8 }
function Try-Step([string]$Name, [scriptblock]$Do) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    try { $r = & $Do; $sw.Stop(); Rec ("STEP {0} OK {1} ms {2}" -f $Name, $sw.ElapsedMilliseconds, ($r | Out-String).Trim().Replace("`r`n",' | ')) }
    catch { $sw.Stop(); Rec ("STEP {0} FAIL {1} ms {2}" -f $Name, $sw.ElapsedMilliseconds, $_.Exception.Message.Replace("`r`n",' ')) }
}
Rec ("START {0:o} as {1}" -f (Get-Date), [Security.Principal.WindowsIdentity]::GetCurrent().Name)
Try-Step 'token has Hyper-V Administrators (S-1-5-32-578)' { [Security.Principal.WindowsIdentity]::GetCurrent().Groups.Value -contains 'S-1-5-32-578' }
Try-Step 'token is elevated (expected False: a filtered/batch token)' { ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator) }
Try-Step 'Get-VM' { @(Get-VM).Count }
Try-Step 'Get-VMHost' { (Get-VMHost | Select-Object -ExpandProperty VirtualMachinePath) }
$swn = 'SWITCHNAME'; $d = 'WORKDIR'
# Hyper-V allows two switches (or VMs) with the same name, and then -SwitchName resolves two objects ("Sequence
# contains more than one element", second probe 2026-11-08). So refuse to run on top of leftovers: nothing is created.
$leftSw = @(Get-VMSwitch -Name $swn -ErrorAction SilentlyContinue).Count
$leftVm = @(Get-VM -Name 'antspawn-v0-vm' -ErrorAction SilentlyContinue).Count
if ($leftSw -gt 0 -or $leftVm -gt 0) {
    Rec ("REFUSED: {0} switch(es) named {1} and {2} VM(s) named antspawn-v0-vm already exist. Nothing was created. Remove them (broker-v0.ps1 -Undo, elevated) and rerun." -f $leftSw, $swn, $leftVm)
    Rec ("END {0:o}" -f (Get-Date))
    exit 3
}
Try-Step 'New-VMSwitch Internal' { New-VMSwitch -Name $swn -SwitchType Internal | Out-Null; (Get-VMSwitch -Name $swn).SwitchType }
Try-Step 'new switch: vEthernet IPv4 Forwarding default' { (Get-NetIPInterface -InterfaceAlias "vEthernet ($swn)" -AddressFamily IPv4).Forwarding }
Try-Step 'new switch: Get-NetNat count (expected 0)' { @(Get-NetNat).Count }
Try-Step 'adapters answering to the alias vEthernet (switch), and the switches of that name' { 'adapters={0} switches={1}' -f @(Get-NetAdapter -Name "vEthernet ($swn)" -ErrorAction SilentlyContinue).Count, @(Get-VMSwitch -Name $swn).Count }
Try-Step 'broker sets the host address itself (first run: Access is denied; second run, two switches existed: OK)' { New-NetIPAddress -InterfaceAlias "vEthernet ($swn)" -IPAddress 10.77.0.9 -PrefixLength 30 | Out-Null; Remove-NetIPAddress -IPAddress 10.77.0.9 -Confirm:$false }
Try-Step 'New-VHD dynamic 64MB parent' { New-VHD -Path "$d\parent.vhdx" -SizeBytes 64MB -Dynamic | Out-Null; (Get-Item "$d\parent.vhdx").Length }
Try-Step 'New-VHD differencing child' { New-VHD -Path "$d\child.vhdx" -ParentPath "$d\parent.vhdx" -Differencing | Out-Null; (Get-Item "$d\child.vhdx").Length }
Try-Step 'New-VM gen2 on the test switch' { New-VM -Name 'antspawn-v0-vm' -Generation 2 -MemoryStartupBytes 256MB -VHDPath "$d\child.vhdx" -SwitchName $swn | Out-Null; (Get-VM 'antspawn-v0-vm').State }
Try-Step 'Set-VMMemory static' { Set-VMMemory 'antspawn-v0-vm' -DynamicMemoryEnabled $false }
Try-Step 'Set-VMProcessor ExposeVirtualizationExtensions false' { Set-VMProcessor 'antspawn-v0-vm' -ExposeVirtualizationExtensions $false; (Get-VMProcessor 'antspawn-v0-vm').ExposeVirtualizationExtensions }
Try-Step 'Disable-VMIntegrationService (all but Heartbeat)' { Get-VMIntegrationService 'antspawn-v0-vm' | Where-Object Name -ne 'Heartbeat' | Disable-VMIntegrationService; (Get-VMIntegrationService 'antspawn-v0-vm' | Where-Object Enabled | Select-Object -ExpandProperty Name) -join ',' }
Try-Step 'Set-VM no checkpoints' { Set-VM 'antspawn-v0-vm' -CheckpointType Disabled; (Get-VM 'antspawn-v0-vm').CheckpointType }
Try-Step 'Start-VM' { Start-VM 'antspawn-v0-vm'; Start-Sleep 3; (Get-VM 'antspawn-v0-vm').State }
Try-Step 'Stop-VM -TurnOff (power cut)' { Stop-VM 'antspawn-v0-vm' -TurnOff -Force; (Get-VM 'antspawn-v0-vm').State }
Try-Step 'Start-VM again, same VM and disk' { Start-VM 'antspawn-v0-vm'; Start-Sleep 3; (Get-VM 'antspawn-v0-vm').State }
Try-Step 'Stop-VM -TurnOff again' { Stop-VM 'antspawn-v0-vm' -TurnOff -Force; (Get-VM 'antspawn-v0-vm').State }
Try-Step 'Remove-VM' { Remove-VM 'antspawn-v0-vm' -Force; @(Get-VM 'antspawn-v0-vm' -ErrorAction SilentlyContinue).Count }
Try-Step 'delete disks' { Remove-Item "$d\child.vhdx","$d\parent.vhdx" -Force; (Get-ChildItem $d -Filter *.vhdx).Count }
Rec ("END {0:o}" -f (Get-Date))
'@
$listen = @'
# Runs AS antspawn-v0-broker for 15 minutes: a one-SID-pair pipe and a TCP listener, logging each client.
$out = 'RESULTS_DIR\listen.txt'
trap { Add-Content -Path $out -Value ("{0:o} FATAL {1}" -f (Get-Date), $_) -Encoding utf8; break }
function Rec([string]$s) { Add-Content -Path $out -Value ("{0:o} {1}" -f (Get-Date), $s) -Encoding utf8 }
Rec ("START as {0}" -f [Security.Principal.WindowsIdentity]::GetCurrent().Name)
$deadline = (Get-Date).AddMinutes(15)
function New-Pipe {
    $ps = New-Object System.IO.Pipes.PipeSecurity
    $ps.SetSecurityDescriptorSddlForm('D:P(A;;FA;;;BROKERSID)(A;;0x12019f;;;RUNNERSID)')
    if ('System.IO.Pipes.NamedPipeServerStreamAcl' -as [type]) {
        [System.IO.Pipes.NamedPipeServerStreamAcl]::Create('antspawn-v0-pipe', [System.IO.Pipes.PipeDirection]::InOut, 1,
            [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::Asynchronous, 4096, 4096, $ps)
    } else {
        New-Object System.IO.Pipes.NamedPipeServerStream('antspawn-v0-pipe', [System.IO.Pipes.PipeDirection]::InOut, 1,
            [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::Asynchronous, 4096, 4096, $ps)
    }
}
$tcp = $null
try { $tcp = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Parse('HOSTIP'), 2222); $tcp.Start(); Rec 'tcp listening HOSTIP:2222' }
catch { Rec ("tcp listen FAIL {0} (the pipe side carries on without TCP)" -f $_.Exception.Message); $tcp = $null }
$pipe = New-Pipe
$wait = $pipe.WaitForConnectionAsync()
Rec 'pipe antspawn-v0-pipe listening'
while ((Get-Date) -lt $deadline) {
    if ($wait.IsCompleted) {
        try {
            $r = New-Object System.IO.StreamReader($pipe); $w = New-Object System.IO.StreamWriter($pipe); $w.AutoFlush = $true
            $line = $r.ReadLine()
            $who = $null
            $pipe.RunAsClient([System.IO.Pipes.PipeStreamImpersonationWorker]{ $script:who = [Security.Principal.WindowsIdentity]::GetCurrent().Name })
            Rec ("pipe client identity={0} said={1}" -f $who, $line)
            $w.WriteLine("hello-from-broker $who")
            $pipe.WaitForPipeDrain()
        } catch { Rec ("pipe handling FAIL {0}" -f $_.Exception.Message) }
        try { $pipe.Dispose() } catch {}
        $pipe = New-Pipe; $wait = $pipe.WaitForConnectionAsync()
    }
    if ($tcp -and $tcp.Pending()) {
        $c = $tcp.AcceptTcpClient(); Rec ("tcp client from {0}" -f $c.Client.RemoteEndPoint); $c.Close()
    }
    Start-Sleep -Milliseconds 200
}
Rec 'END'
'@

Write-Host ("mode: {0}" -f $(if ($Apply) { 'APPLY' } elseif ($Undo) { 'UNDO' } else { 'DRY RUN (nothing changes)' }))
if ($Undo) {
    foreach ($t in 'AntSpawnV0Probe', 'AntSpawnV0Listen') {
        Step "stop and unregister task $t" {
            if (Get-ScheduledTask -TaskName $t -ErrorAction SilentlyContinue) {
                Stop-ScheduledTask -TaskName $t -ErrorAction SilentlyContinue
                Unregister-ScheduledTask -TaskName $t -Confirm:$false
            }
        }
    }
    Step "remove any antspawn-v0-* VM, its disks, the host address and the switch $SwitchName" {
        Get-VM -Name 'antspawn-v0-*' -ErrorAction SilentlyContinue | ForEach-Object { Stop-VM $_ -TurnOff -Force -ErrorAction SilentlyContinue; Remove-VM $_ -Force }
        Get-NetIPAddress -IPAddress $HostIp -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false
        Get-VMSwitch -Name $SwitchName -ErrorAction SilentlyContinue | Remove-VMSwitch -Force
    }
    Step "remove $Broker from Hyper-V Administrators, its batch right, and the user" {
        $u = Get-LocalUser -Name $Broker -ErrorAction SilentlyContinue
        if ($u) {
            $sid = $u.SID.Value
            Remove-LocalGroupMember -SID 'S-1-5-32-578' -Member $Broker -ErrorAction SilentlyContinue
            $have = @(Get-BatchRightSids)
            if ($have -contains "*$sid") { Set-BatchRight @($have | Where-Object { $_ -ne "*$sid" }) }
            Remove-LocalUser -Name $Broker
        }
    }
    Step "delete $Dir" { if (Test-Path $Dir) { Remove-Item $Dir -Recurse -Force } }
    Check "no antspawn-v0 tasks" { -not (Get-ScheduledTask -TaskName 'AntSpawnV0*' -ErrorAction SilentlyContinue) }
    Check "no antspawn-v0 VM" { -not (Get-VM -Name 'antspawn-v0-*' -ErrorAction SilentlyContinue) }
    Check "switch $SwitchName gone" { -not (Get-VMSwitch -Name $SwitchName -ErrorAction SilentlyContinue) }
    Check "$HostIp not on any interface" { -not (Get-NetIPAddress -IPAddress $HostIp -ErrorAction SilentlyContinue) }
    Check "$Broker user gone" { -not (Get-LocalUser -Name $Broker -ErrorAction SilentlyContinue) }
    Check "Hyper-V Administrators has no $Broker" { -not (Get-LocalGroupMember -SID 'S-1-5-32-578' | Where-Object { $_.Name -like "*$Broker" }) }
    Check "$Dir gone" { -not (Test-Path $Dir) }
} else {
    $runner = Get-LocalUser -Name $RunnerUser -ErrorAction SilentlyContinue
    if ($Live -and -not $runner) { throw "runner account $RunnerUser not found (run account-setup.ps1 first)" }
    $runnerSid = if ($runner) { $runner.SID.Value } else { '<sid of ' + $RunnerUser + '>' }
    $launcherSid = Sid $LauncherUser
    if ($Live) {
        $leftSw = @(Get-VMSwitch -Name $SwitchName -ErrorAction SilentlyContinue).Count
        $leftVm = @(Get-VM -Name 'antspawn-v0-vm' -ErrorAction SilentlyContinue).Count
        if ($leftSw -gt 0 -or $leftVm -gt 0) { throw "$leftSw switch(es) named $SwitchName and $leftVm VM(s) named antspawn-v0-vm exist from an earlier run (Hyper-V allows duplicates, and a second one breaks New-VM -SwitchName). Run -Undo first. Nothing was changed." }
    }
    elseif (-not (Get-Command Get-VMSwitch -ErrorAction SilentlyContinue)) { Write-Host 'note: the Hyper-V cmdlets are not available here; -Apply would need them' }
    $plain = $null
    Step "create folder $Dir (Administrators and SYSTEM full, $Broker full control on results\ only, $LauncherUser read)" {
        New-Item -ItemType Directory -Force -Path $Res | Out-Null
    }
    Step "create local user $Broker (random password, never expires), NO other right than the two below" {
        $bytes = [byte[]]::new(48); $rng = [Security.Cryptography.RandomNumberGenerator]::Create(); $rng.GetBytes($bytes); $rng.Dispose()
        $script:plain = [Convert]::ToBase64String($bytes) + 'aA1'
        $sp = ConvertTo-SecureString $script:plain -AsPlainText -Force
        if (Get-LocalUser -Name $Broker -ErrorAction SilentlyContinue) { Set-LocalUser -Name $Broker -Password $sp }
        else { New-LocalUser -Name $Broker -Password $sp -PasswordNeverExpires -UserMayNotChangePassword -AccountNeverExpires -Description $BrokerDescription | Out-Null }
    }
    $bsid = if ($Live) { (Get-LocalUser -Name $Broker).SID.Value } else { "<sid of $Broker>" }
    Step "ACL $Dir" {
        # (OI)(CI) so the files created inside inherit the broker's read: without it the broker could open
        # the folder but not probe.ps1, and powershell exited 1 before writing anything (first -Apply run).
        & icacls.exe $Dir /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "*${launcherSid}:(OI)(CI)RX" "*${bsid}:(OI)(CI)RX" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'icacls failed on the folder' }
        # Full control, not Modify, and on this folder only: New-VM stopped with 'failed to set folder permission for results (0x80070005)' because
        # Hyper-V grants the VM's own SID on the disk's folder as the caller, and Modify has no WRITE_DAC (first probe, 2026-11-08).
        & icacls.exe $Res /grant:r "*${bsid}:(OI)(CI)F" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'icacls failed on results' }
    }
    Step "add $Broker to Hyper-V Administrators (S-1-5-32-578)" {
        if (-not (Get-LocalGroupMember -SID 'S-1-5-32-578' | Where-Object { $_.Name -like "*$Broker" })) { Add-LocalGroupMember -SID 'S-1-5-32-578' -Member $Broker }
    }
    Step "grant $Broker 'Log on as a batch job'" {
        $have = @(Get-BatchRightSids)
        if ($have -notcontains "*$bsid") { Set-BatchRight (@($have) + "*$bsid") }
    }
    Step "write probe.ps1 and listen.ps1 (read-only for the broker)" {
        Set-Content -Path (Join-Path $Dir 'probe.ps1') -Encoding utf8 -Value ($probe.Replace('RESULTS_DIR', $Res).Replace('SWITCHNAME', $SwitchName).Replace('WORKDIR', $Res))
        Set-Content -Path (Join-Path $Dir 'listen.ps1') -Encoding utf8 -Value ($listen.Replace('RESULTS_DIR', $Res).Replace('BROKERSID', $bsid).Replace('RUNNERSID', $runnerSid).Replace('HOSTIP', $HostIp))
    }
    Step "read the ACL of probe.ps1 and listen.ps1 back and fail unless $Broker can read them (a granted ACL is not a read ACL until read back)" {
        foreach ($f in 'probe.ps1', 'listen.ps1') {
            $path = Join-Path $Dir $f
            & icacls.exe $path
            $mine = @((Get-Acl -LiteralPath $path).Access | Where-Object {
                $_.AccessControlType -eq 'Allow' -and
                $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value -eq $bsid -and
                ($_.FileSystemRights -band [Security.AccessControl.FileSystemRights]::ReadAndExecute) -eq [Security.AccessControl.FileSystemRights]::ReadAndExecute })
            if ($mine.Count -lt 1) { throw "$Broker has no read ACE on $path; the probe task would exit 1 before writing anything. Run -Undo." }
            Write-Host "OK     $f is readable by $Broker (ACE read back)"
        }
        $resAcl = @((Get-Acl -LiteralPath $Res).Access | Where-Object {
            $_.AccessControlType -eq 'Allow' -and
            $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value -eq $bsid -and
            ($_.FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -eq [Security.AccessControl.FileSystemRights]::FullControl })
        if ($resAcl.Count -lt 1) { throw "$Broker has no full-control ACE on $Res; New-VM would fail setting the VM's folder permission. Run -Undo." }
        Write-Host "OK     $Res gives $Broker full control (ACE read back)"
    }
    $sd = "D:(A;;FA;;;BA)(A;;FA;;;SY)(A;;GRGX;;;$launcherSid)"
    foreach ($t in @(@('AntSpawnV0Probe', 'probe.ps1'), @('AntSpawnV0Listen', 'listen.ps1'))) {
        Step "register task $($t[0]) AS $Broker (batch logon); $LauncherUser may run it" {
            $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$(Join-Path $Dir $t[1])`""
            $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 30) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
            Register-ScheduledTask -TaskName $t[0] -InputObject (New-ScheduledTask -Action $action -Settings $settings) -User "$env:COMPUTERNAME\$Broker" -Password $script:plain -Force | Out-Null
            $svc = New-Object -ComObject Schedule.Service; $svc.Connect()
            $svc.GetFolder('\').GetTask($t[0]).SetSecurityDescriptor($sd, 0)
        }
    }
    $plain = $null
    Step "run AntSpawnV0Probe once and wait (up to 5 min), then set $HostIp/30 on the new switch's vEthernet (elevated: the broker cannot)" {
        $leftSw = @(Get-VMSwitch -Name $SwitchName -ErrorAction SilentlyContinue).Count
        $leftVm = @(Get-VM -Name 'antspawn-v0-vm' -ErrorAction SilentlyContinue).Count
        if ($leftSw -gt 0 -or $leftVm -gt 0) { throw "$leftSw switch(es) named $SwitchName and $leftVm VM(s) named antspawn-v0-vm already exist from an earlier run; the probe would refuse. Run -Undo first. Nothing new was started." }
        $now = Get-Date; $since = $now.AddTicks(-($now.Ticks % [TimeSpan]::TicksPerSecond))
        Start-ScheduledTask -TaskName 'AntSpawnV0Probe'
        $info = Wait-Task 'AntSpawnV0Probe' $since 300
        Write-Host ("probe task LastTaskResult = 0x{0:X}" -f $info.LastTaskResult)
        $if = "vEthernet ($SwitchName)"
        if (Get-NetAdapter -Name $if -ErrorAction SilentlyContinue) {
            if (-not (Get-NetIPAddress -IPAddress $HostIp -ErrorAction SilentlyContinue)) { New-NetIPAddress -InterfaceAlias $if -IPAddress $HostIp -PrefixLength 30 | Out-Null }
            # a new address is Tentative for a moment and cannot be bound until it is Preferred (the first listener died on that)
            $until = (Get-Date).AddSeconds(30)
            while ((Get-NetIPAddress -IPAddress $HostIp -ErrorAction SilentlyContinue).AddressState -ne 'Preferred' -and (Get-Date) -lt $until) { Start-Sleep -Milliseconds 500 }
            $state = (Get-NetIPAddress -IPAddress $HostIp -ErrorAction SilentlyContinue).AddressState
            Write-Host "$HostIp AddressState = $state"
            if ($state -ne 'Preferred') { throw "$HostIp did not become Preferred within 30 s; do not start the listener yet." }
        } else { Write-Host "no adapter $if (the broker could not create the switch); see probe.txt" }
    }
    if ($Live) {
        Write-Host "`n----- results\probe.txt -----"
        Get-Content (Join-Path $Res 'probe.txt') -ErrorAction SilentlyContinue
        Check "probe wrote results" { Test-Path (Join-Path $Res 'probe.txt') }
        Check "$Broker is in Hyper-V Administrators" { Get-LocalGroupMember -SID 'S-1-5-32-578' | Where-Object { $_.Name -like "*$Broker" } }
        Check "$Broker holds SeBatchLogonRight" { @(Get-BatchRightSids) -contains "*$bsid" }
        Write-Host "`nNext (the lane does this, no elevation): Start-ScheduledTask AntSpawnV0Listen, then runs the client AS $RunnerUser. Roll back with: .\broker-v0.ps1 -Undo"
    }
}
if ($Live) {
    Write-Host ''
    $results | Format-Table -AutoSize | Out-String | Write-Host
    if ($results | Where-Object { -not $_.Ok }) { Write-Host 'FAIL: at least one property did not hold.'; exit 1 }
    Write-Host 'PASS'
}
