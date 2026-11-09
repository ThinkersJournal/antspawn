# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  The VM broker's pool of Hyper-V switches (docs/SANDBOX-BROKER.md, slice V3): N Internal switches, one
  per VM slot, each with a fixed host-side address, IPv4 forwarding Disabled, no NAT, and a Windows
  Firewall rule that blocks everything inbound on that switch's interface. Elevated, run by the machine
  owner, never by a lane. DRY-RUN BY DEFAULT; -Apply creates, -Undo removes what -Apply recorded.

.DESCRIPTION
  Slot i (1..N) is: switch `antspawn-sw<i>`, host address 10.77.<i>.1/30, guest address 10.77.<i>.2.
  Part B (account-firewall.ps1) already allows a runner's TCP 22 to 10.77.0.0/16, so these addresses are
  inside what it allows and nothing else needs changing.

  -Apply, in this order:
    1. refuse if anything already answers to a 10.77.x address (a leftover scripts\broker-v0.ps1 switch
       uses 10.77.0.1: run its -Undo first), if any `antspawn-sw<i>` switch or `antspawn-pool-in-<i>`
       rule already exists, or if state.json exists. Nothing is created in a refused run.
    2. for each slot: New-VMSwitch -SwitchType Internal, set the host address, Set-NetIPInterface
       -Forwarding Disabled, wait (30 s) for the address to be Preferred, add the inbound Block rule on the
       interface, and READ ALL OF IT BACK; stop at the first mismatch.
    3. write state.json (the slots this script made) into %ProgramData%\antspawn-pool, whose ACL is
       Administrators and SYSTEM only and is read back.
  A failure part-way leaves the slots already made, listed in state.json if step 3 was reached and
  otherwise printed; -Undo (or the printed names) removes them. Nothing is half-recorded silently: the
  state file is written after EACH slot, so -Undo always knows what exists.

  -Undo removes only the switches and rules named in state.json AND carrying our prefixes, then reads
  back that they are gone, then removes state.json. A VM still attached to a pool switch makes Remove-VMSwitch
  fail; that is reported and the entry stays in state.json.

  Not exercised by the dry run and able to fail: New-VMSwitch, New-NetIPAddress on the new vEthernet,
  New-NetFirewallRule -InterfaceAlias for an interface that appeared seconds ago, and the icacls. The
  inbound Block means a guest cannot reach any host service at its switch's host address (design, not
  yet measured: the guest probe is V6).
#>
[CmdletBinding()]
param(
    [switch]$Apply,
    [switch]$Undo,
    [ValidateRange(1, 32)][int]$Slots = 4
)
$ErrorActionPreference = 'Stop'
if ($Apply -and $Undo) { throw 'Choose -Apply or -Undo, not both.' }
$Live = $Apply -or $Undo
$SwitchPrefix = 'antspawn-sw'
$RulePrefix = 'antspawn-pool-in-'
$Dir = Join-Path $env:ProgramData 'antspawn-pool'
$StateFile = Join-Path $Dir 'state.json'
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if ($Live -and -not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this in an elevated PowerShell for -Apply or -Undo (the dry run needs none).'
}

function Slot-Plan([int]$i) {
    [pscustomobject]@{
        Index  = $i
        Switch = "$SwitchPrefix$i"
        Alias  = "vEthernet ($SwitchPrefix$i)"
        Rule   = "$RulePrefix$i"
        Host   = "10.77.$i.1"
        Guest  = "10.77.$i.2"
        Prefix = 30
    }
}
$Plan = 1..$Slots | ForEach-Object { Slot-Plan $_ }

function Save-State($made) {
    $json = ConvertTo-Json -InputObject @($made) -Depth 3
    Set-Content -LiteralPath $StateFile -Value $json -Encoding UTF8
}
function Read-State {
    if (-not (Test-Path -LiteralPath $StateFile)) { return @() }
    $raw = Get-Content -LiteralPath $StateFile -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) { return @() }
    return @($raw | ConvertFrom-Json)
}

function Verify-Slot($p) {
    $problems = @()
    $sw = @(Get-VMSwitch -Name $p.Switch -ErrorAction SilentlyContinue)
    if ($sw.Count -ne 1) { $problems += "switches named $($p.Switch): $($sw.Count), want 1" }
    elseif ($sw[0].SwitchType -ne 'Internal') { $problems += "switch type $($sw[0].SwitchType), want Internal" }
    $ad = @(Get-NetIPAddress -InterfaceAlias $p.Alias -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        Where-Object { $_.IPAddress -eq $p.Host })
    if ($ad.Count -ne 1) { $problems += "address $($p.Host) on $($p.Alias): $($ad.Count), want 1" }
    else {
        if ($ad[0].PrefixLength -ne $p.Prefix) { $problems += "prefix $($ad[0].PrefixLength), want $($p.Prefix)" }
        if ($ad[0].AddressState -ne 'Preferred') { $problems += "address state $($ad[0].AddressState), want Preferred" }
    }
    $fw = (Get-NetIPInterface -InterfaceAlias $p.Alias -AddressFamily IPv4 -ErrorAction SilentlyContinue).Forwarding
    if ("$fw" -ne 'Disabled') { $problems += "IPv4 forwarding '$fw', want Disabled" }
    $r = @(Get-NetFirewallRule -Name $p.Rule -ErrorAction SilentlyContinue)
    if ($r.Count -ne 1) { $problems += "firewall rules named $($p.Rule): $($r.Count), want 1" }
    else {
        if ($r[0].Direction -ne 'Inbound' -or $r[0].Action -ne 'Block' -or $r[0].Enabled -ne 'True') {
            $problems += "rule is $($r[0].Direction)/$($r[0].Action)/enabled=$($r[0].Enabled), want Inbound/Block/True"
        }
        $if = Get-NetFirewallInterfaceFilter -AssociatedNetFirewallRule $r[0]
        if (@($if.InterfaceAlias) -notcontains $p.Alias) { $problems += "rule interface filter is '$($if.InterfaceAlias)', want $($p.Alias)" }
    }
    return $problems
}

Write-Host ("mode: " + $(if ($Apply) { 'APPLY' } elseif ($Undo) { 'UNDO' } else { 'DRY RUN (nothing is changed)' }))

if (-not $Undo) {
    foreach ($p in $Plan) {
        Write-Host ("{0,-6} switch {1} Internal; host {2}/{3} (guest {4}); forwarding Disabled; no NAT; inbound Block rule {5} on '{6}'" -f 'PLAN', $p.Switch, $p.Host, $p.Prefix, $p.Guest, $p.Rule, $p.Alias)
    }
    Write-Host "PLAN   state: $StateFile (Administrators and SYSTEM only, ACL read back), written after each slot"
}

if ($Undo) {
    $made = @(Read-State)
    if ($made.Count -eq 0) { Write-Host 'nothing recorded in state.json: nothing to undo'; return }
    $left = @()
    foreach ($m in $made) {
        if (-not ($m.Switch -like "$SwitchPrefix*") -or -not ($m.Rule -like "$RulePrefix*")) {
            Write-Warning "state.json entry '$($m.Switch)' does not carry our prefixes; left alone"
            $left += $m
            continue
        }
        try {
            if (Get-NetFirewallRule -Name $m.Rule -ErrorAction SilentlyContinue) { Remove-NetFirewallRule -Name $m.Rule }
            if (Get-VMSwitch -Name $m.Switch -ErrorAction SilentlyContinue) { Remove-VMSwitch -Name $m.Switch -Force }
            $still = (Get-NetFirewallRule -Name $m.Rule -ErrorAction SilentlyContinue) -or (Get-VMSwitch -Name $m.Switch -ErrorAction SilentlyContinue)
            if ($still) { throw 'still present after removal' }
            Write-Host "OK     removed $($m.Switch) and rule $($m.Rule) (read back: gone)"
        } catch {
            Write-Warning "could not remove $($m.Switch): $($_.Exception.Message); it stays in state.json"
            $left += $m
        }
    }
    if ($left.Count -eq 0) { Remove-Item -LiteralPath $StateFile -Force; Write-Host 'OK     state.json removed' }
    else { Save-State $left; Write-Host "$($left.Count) entries remain in state.json" }
    return
}

# Refusals (dry run reports them too, creating nothing).
$refuse = @()
$taken = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object { $_.IPAddress -like '10.77.*' })
if ($taken.Count -gt 0) {
    $refuse += ("something already answers to a 10.77.x address ({0}); run scripts\broker-v0.ps1 -Undo if that is its switch" -f (($taken | ForEach-Object { "$($_.IPAddress) on $($_.InterfaceAlias)" }) -join ', '))
}
foreach ($p in $Plan) {
    if (@(Get-VMSwitch -Name $p.Switch -ErrorAction SilentlyContinue).Count -gt 0) { $refuse += "a switch named $($p.Switch) already exists" }
    if (Get-NetFirewallRule -Name $p.Rule -ErrorAction SilentlyContinue) { $refuse += "a firewall rule named $($p.Rule) already exists" }
}
if (Test-Path -LiteralPath $StateFile) { $refuse += "$StateFile exists: -Undo first" }
foreach ($r in $refuse) { Write-Host "REFUSE $r" }
if ($refuse.Count -gt 0) { if ($Apply) { throw 'Refused; nothing was created.' } else { Write-Host 'a -Apply now would be REFUSED (above)' ; return } }
if (-not $Apply) { Write-Host 'Dry run complete. -Apply creates the above; -Undo removes it.'; return }

if (-not (Test-Path -LiteralPath $Dir)) { New-Item -ItemType Directory -Path $Dir | Out-Null }
& icacls.exe $Dir /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'icacls failed on the state folder' }
$acl = Get-Acl -LiteralPath $Dir
$ids = @($acl.Access | ForEach-Object { $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value } | Sort-Object -Unique)
$bad = @($ids | Where-Object { $_ -notin @('S-1-5-32-544', 'S-1-5-18') })
if ($bad.Count -gt 0 -or $ids.Count -ne 2) { throw "state folder ACL is not Administrators+SYSTEM only: $($ids -join ', ')" }
Write-Host 'OK     state folder ACL read back: Administrators and SYSTEM only'

$made = @()
foreach ($p in $Plan) {
    Write-Host "DO     slot $($p.Index): $($p.Switch)"
    New-VMSwitch -Name $p.Switch -SwitchType Internal | Out-Null
    $made += [pscustomobject]@{ Switch = $p.Switch; Rule = $p.Rule; Host = $p.Host }
    Save-State $made
    New-NetIPAddress -InterfaceAlias $p.Alias -IPAddress $p.Host -PrefixLength $p.Prefix | Out-Null
    Set-NetIPInterface -InterfaceAlias $p.Alias -AddressFamily IPv4 -Forwarding Disabled
    New-NetFirewallRule -Name $p.Rule -DisplayName "antspawn pool slot $($p.Index): block inbound" -Direction Inbound -Action Block -InterfaceAlias $p.Alias -Profile Any | Out-Null
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        $st = (Get-NetIPAddress -InterfaceAlias $p.Alias -IPAddress $p.Host -ErrorAction SilentlyContinue).AddressState
        if ($st -eq 'Preferred') { break }
        Start-Sleep -Milliseconds 500
    }
    $problems = Verify-Slot $p
    if ($problems.Count -gt 0) {
        throw ("slot $($p.Index) read back wrong: " + ($problems -join '; ') + ". Made so far is listed in $StateFile; .\broker-pool.ps1 -Undo removes it.")
    }
    Write-Host "OK     slot $($p.Index) read back: Internal, $($p.Host)/$($p.Prefix) Preferred, forwarding Disabled, inbound Block rule bound to $($p.Alias)"
}
$nat = @(Get-NetNat -ErrorAction SilentlyContinue).Count
Write-Host "INFO   NAT objects on this machine: $nat (this script creates none)"
Write-Host "DONE   $Slots slots. Roll back with: .\broker-pool.ps1 -Undo"
