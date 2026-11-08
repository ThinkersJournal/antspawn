# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Part B of the account network boundary (docs/ACCOUNT-ISOLATION.md): replace the blanket
  "block all outbound" rule for the restricted account with "block outbound EXCEPT TCP 22 to the VM
  pool range", so a runner can SSH to its own VM and still reaches nothing else outside the machine.
  Elevated, run by the machine owner, never by a lane. DRY-RUN BY DEFAULT; -Undo restores.

.DESCRIPTION
  What it is for, measured 2026-11-08: the rule `antspawn-run: block all outbound` (account-setup.ps1)
  also blocks the VM broker's guests (a guest at 10.77.0.2 is a remote address, and Block beats Allow),
  so a runner cannot SSH to its VM while it stands. This script does NOT touch loopback or the host's own
  addresses: Windows Firewall rules do not see them (the existing rule is the proof). That is Part A
  (a separate script, WFP filters).

  -Apply, in this order, so the account is never unprotected:
    1. create three rules, all named `antspawn-fw-*`, all Outbound + Block + Profile Any + LocalUser = the
       account's SID:
         antspawn-fw-block-outside-pool      every protocol, every remote address EXCEPT 10.77.0.0/16
                                             (two IPv4 ranges + all of IPv6)
         antspawn-fw-block-pool-tcp-not-ssh  TCP to 10.77.0.0/16, remote ports 1-21 and 23-65535
         antspawn-fw-block-pool-udp          UDP to 10.77.0.0/16
       ICMP and other protocols to the pool are NOT blocked (stated, not measured).
    2. READ EACH RULE BACK (Get-NetFirewallRule + the security, address and port filters) and refuse on
       any mismatch, BEFORE touching the old rule. A mismatch leaves the old rule in force.
    3. only then DISABLE (not delete) `antspawn-run: block all outbound`, recording that it was enabled in
       %ProgramData%\antspawn-fw\state.json (Administrators and SYSTEM only, ACL read back), and read the
       disabled state back.
  -Undo: re-enables the old rule ONLY if state.json says this script disabled it, removes only rules named
  `antspawn-fw-*`, removes state.json, and reads all of it back.

  The exceptions are derived from what the VM broker path needs (SSH to a guest at a pool address,
  docs/SANDBOX-BROKER.md), not from a guess. After -Apply the lane re-runs its connect-only checks AS the
  account (scripts/account-reach-*.ps1 through `spawn run`) and records them; this script does not claim
  the result.

  Elevated-only properties that a dry run cannot exercise and that could still fail: New-NetFirewallRule
  accepting the IPv6 range at creation (it accepted it at parameter binding), Set-NetFirewallRule
  -Enabled False on the old rule, the folder ACL (icacls), and the canonical form Get-NetFirewallAddressFilter
  returns (compared as address ranges, not as text). A failure before step 3 leaves the old rule in force.
#>
[CmdletBinding()]
param(
    [switch]$Apply,
    [switch]$Undo,
    [string]$AccountName = 'antspawn-run',
    [string]$OldRuleName = 'antspawn-run: block all outbound',
    [string]$PoolCidr = '10.77.0.0/16'
)
$ErrorActionPreference = 'Stop'
if ($Apply -and $Undo) { throw 'Choose -Apply or -Undo, not both.' }
$Live = $Apply -or $Undo
$Prefix = 'antspawn-fw-'
$Dir = Join-Path $env:ProgramData 'antspawn-fw'
$StateFile = Join-Path $Dir 'state.json'
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if ($Live -and -not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this in an elevated PowerShell for -Apply or -Undo (the dry run needs none).'
}

# --- the rules, as data, so the dry run prints exactly what -Apply creates and the read-back compares the same data ---
# The pool's complement for IPv4 (10.77.0.0/16 = 10.77.0.0-10.77.255.255), and all of IPv6.
$PoolV4 = '10.77.0.0-10.77.255.255'
$OutsideV4 = @('0.0.0.0-10.76.255.255', '10.78.0.0-255.255.255.255')
$AllV6 = '::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff'
$Rules = @(
    @{ Name = "${Prefix}block-outside-pool"; Protocol = 'Any'; RemoteAddress = ($OutsideV4 + $AllV6); RemotePort = @('Any');
       Description = 'antspawn account: block outbound except the VM pool (Part B)' },
    @{ Name = "${Prefix}block-pool-tcp-not-ssh"; Protocol = 'TCP'; RemoteAddress = @($PoolCidr); RemotePort = @('1-21', '23-65535');
       Description = 'antspawn account: TCP to the VM pool only on port 22 (Part B)' },
    @{ Name = "${Prefix}block-pool-udp"; Protocol = 'UDP'; RemoteAddress = @($PoolCidr); RemotePort = @('Any');
       Description = 'antspawn account: no UDP to the VM pool (Part B)' }
)

# --- argument limits, checked in the dry run too (a first -Apply of the broker script stopped on a 48-char limit) ---
foreach ($r in $Rules) {
    if ($r.Name.Length -gt 60) { throw "rule name $($r.Name) is $($r.Name.Length) characters (kept under 60). Nothing was changed." }
    if ($r.Description.Length -gt 200) { throw "description of $($r.Name) is $($r.Description.Length) characters (kept under 200). Nothing was changed." }
    if ($r.Name -notmatch '^[A-Za-z0-9._-]+$') { throw "rule name $($r.Name) has a character outside A-Za-z0-9._-. Nothing was changed." }
}
if ($AccountName.Length -gt 20) { throw "account name is longer than 20 characters. Nothing was changed." }
if ($PoolCidr -notmatch '^\d{1,3}(\.\d{1,3}){3}/\d{1,2}$') { throw "PoolCidr $PoolCidr is not a CIDR. Nothing was changed." }

# --- helpers ---
$results = [System.Collections.Generic.List[object]]::new()
function Step([string]$What, [scriptblock]$Do) { if ($Live) { Write-Host "DO     $What"; & $Do } else { Write-Host "WOULD  $What" } }
function Check([string]$Property, [scriptblock]$Test) {
    $ok = $false
    try { $ok = [bool](& $Test) } catch { $ok = $false }
    $results.Add([pscustomobject]@{ Property = $Property; Ok = $ok })
}
function ConvertTo-V4Number([string]$ip) { $b = ([ipaddress]$ip).GetAddressBytes(); [uint64]($b[0] * 16777216 + $b[1] * 65536 + $b[2] * 256 + $b[3]) }
# One canonical text per address entry, so the cmdlet's own spelling does not matter: IPv4 as `first-last` decimal, IPv6 as `first-last` canonical.
function ConvertTo-CanonicalRange([string]$entry) {
    $e = $entry.Trim()
    if ($e -match '^(\d{1,3}(?:\.\d{1,3}){3})/(\d{1,2})$') {
        $len = [int]$Matches[2]; $base = ConvertTo-V4Number $Matches[1]
        $size = [math]::Pow(2, 32 - $len); $first = [math]::Floor($base / $size) * $size
        return ('v4:{0}-{1}' -f [uint64]$first, [uint64]($first + $size - 1))
    }
    if ($e -match '^(\d{1,3}(?:\.\d{1,3}){3})-(\d{1,3}(?:\.\d{1,3}){3})$') { return ('v4:{0}-{1}' -f (ConvertTo-V4Number $Matches[1]), (ConvertTo-V4Number $Matches[2])) }
    if ($e -match '^\d{1,3}(?:\.\d{1,3}){3}$') { $n = ConvertTo-V4Number $e; return "v4:$n-$n" }
    if ($e -eq '::/0') { return 'v6:' + ([ipaddress]'::').ToString() + '-' + ([ipaddress]'ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff').ToString() }
    if ($e -match '^([0-9a-fA-F:]+)-([0-9a-fA-F:]+)$') { return 'v6:' + ([ipaddress]$Matches[1]).ToString() + '-' + ([ipaddress]$Matches[2]).ToString() }
    return "other:$e"
}
function ConvertTo-CanonicalSet($list) { @($list | ForEach-Object { ConvertTo-CanonicalRange $_ } | Sort-Object -Unique) }
function Get-RuleFacts([string]$Name) {
    $r = Get-NetFirewallRule -Name $Name -ErrorAction Stop
    [pscustomobject]@{
        Enabled = [string]$r.Enabled; Direction = [string]$r.Direction; Action = [string]$r.Action; Profile = [string]$r.Profile
        LocalUser = ($r | Get-NetFirewallSecurityFilter).LocalUser
        RemoteAddress = @(($r | Get-NetFirewallAddressFilter).RemoteAddress)
        Protocol = [string]($r | Get-NetFirewallPortFilter).Protocol
        RemotePort = @(($r | Get-NetFirewallPortFilter).RemotePort)
    }
}
function Assert-RuleMatches($want, [string]$sid) {
    $got = Get-RuleFacts $want.Name
    $problems = [System.Collections.Generic.List[string]]::new()
    if ($got.Enabled -ne 'True') { $problems.Add("Enabled=$($got.Enabled)") }
    if ($got.Direction -ne 'Outbound') { $problems.Add("Direction=$($got.Direction)") }
    if ($got.Action -ne 'Block') { $problems.Add("Action=$($got.Action)") }
    if ($got.Profile -ne 'Any') { $problems.Add("Profile=$($got.Profile)") }
    if ("$($got.LocalUser)" -notlike "*$sid*") { $problems.Add("LocalUser=$($got.LocalUser) does not name $sid") }
    if ($got.Protocol -ne $want.Protocol) { $problems.Add("Protocol=$($got.Protocol) want $($want.Protocol)") }
    $wantAddr = ConvertTo-CanonicalSet $want.RemoteAddress; $gotAddr = ConvertTo-CanonicalSet $got.RemoteAddress
    if ((Compare-Object $wantAddr $gotAddr) -ne $null) { $problems.Add("RemoteAddress got [$($got.RemoteAddress -join ', ')] want [$($want.RemoteAddress -join ', ')]") }
    if ((Compare-Object (@($want.RemotePort) | Sort-Object) (@($got.RemotePort) | Sort-Object)) -ne $null) { $problems.Add("RemotePort got [$($got.RemotePort -join ', ')] want [$($want.RemotePort -join ', ')]") }
    if ($problems.Count) { throw "rule $($want.Name) read back wrong: $($problems -join '; ')" }
    Write-Host "OK     $($want.Name) read back: Outbound Block, SID filter names $sid, protocol $($got.Protocol), address and port filters match"
}

$account = Get-LocalUser -Name $AccountName -ErrorAction SilentlyContinue
if ($Live -and -not $account) { throw "account $AccountName not found (run account-setup.ps1 first). Nothing was changed." }
$sid = if ($account) { $account.SID.Value } else { "<sid of $AccountName>" }
$sddl = "D:(A;;CC;;;$sid)"
Write-Host ("mode: {0}" -f $(if ($Apply) { 'APPLY' } elseif ($Undo) { 'UNDO' } else { 'DRY RUN (nothing changes)' }))

if ($Undo) {
    $state = if (Test-Path $StateFile) { Get-Content $StateFile -Raw | ConvertFrom-Json } else { $null }
    Step "remove every rule named $Prefix*" {
        Get-NetFirewallRule -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "$Prefix*" } | Remove-NetFirewallRule
    }
    Step "re-enable '$OldRuleName' ONLY if state.json says this script disabled it" {
        if ($state -and $state.disabledByUs -eq $true) { Set-NetFirewallRule -Name $state.oldRuleName -Enabled True }
        else { Write-Host "       state.json says this script did not disable it (or is missing); the old rule is left as it is" }
    }
    Step "remove $Dir" { if (Test-Path $Dir) { Remove-Item -LiteralPath $Dir -Recurse -Force } }
    if ($Live) {
        Check "no rule named $Prefix* remains" { -not (Get-NetFirewallRule -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "$Prefix*" }) }
        if ($state -and $state.disabledByUs -eq $true) { Check "old rule '$($state.oldRuleName)' is enabled again" { (Get-NetFirewallRule -Name $state.oldRuleName).Enabled -eq 'True' } }
        Check "$Dir gone" { -not (Test-Path $Dir) }
    }
} else {
    $old = Get-NetFirewallRule -Name $OldRuleName -ErrorAction SilentlyContinue
    if (-not $old) { $old = Get-NetFirewallRule -DisplayName $OldRuleName -ErrorAction SilentlyContinue | Select-Object -First 1 }
    if ($Live -and -not $old) { throw "the old rule '$OldRuleName' was not found, so there is nothing to replace. Nothing was changed." }
    if ($Live -and (Get-NetFirewallRule -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "$Prefix*" })) { throw "rules named $Prefix* already exist; run -Undo first. Nothing was changed." }
    foreach ($r in $Rules) {
        Step ("create rule {0}: Outbound Block, Profile Any, LocalUser {1}, Protocol {2}, RemoteAddress {3}, RemotePort {4}" -f $r.Name, $sddl, $r.Protocol, ($r.RemoteAddress -join ', '), ($r.RemotePort -join ', ')) {
            $p = @{ Name = $r.Name; DisplayName = $r.Name; Description = $r.Description; Direction = 'Outbound'; Action = 'Block'; Profile = 'Any'
                    LocalUser = $sddl; Protocol = $r.Protocol; RemoteAddress = $r.RemoteAddress }
            if ($r.RemotePort -ne @('Any')) { $p.RemotePort = $r.RemotePort }
            New-NetFirewallRule @p | Out-Null
        }
    }
    Step "read each of the three rules back (rule + security, address and port filters) and refuse on any mismatch; the old rule is still in force until this passes" {
        foreach ($r in $Rules) { Assert-RuleMatches $r $sid }
    }
    Step "create $Dir (Administrators and SYSTEM only) and write state.json: old rule '$OldRuleName' was enabled" {
        New-Item -ItemType Directory -Force -Path $Dir | Out-Null
        & icacls.exe $Dir /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'icacls failed on the state folder. The old rule is still in force; run -Undo.' }
        $wasEnabled = ($old.Enabled -eq 'True')
        ([pscustomobject]@{ oldRuleName = $old.Name; disabledByUs = $false; wasEnabled = $wasEnabled; at = (Get-Date -Format o) } | ConvertTo-Json) | Set-Content -Path $StateFile -Encoding ascii
        $acl = (Get-Acl -LiteralPath $Dir).Access | ForEach-Object { $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value }
        $extra = @($acl | Where-Object { $_ -notin 'S-1-5-18', 'S-1-5-32-544' })
        if ($extra.Count) { throw "the state folder ACL names others than SYSTEM and Administrators: $($extra -join ', '). The old rule is still in force; run -Undo." }
        Write-Host "OK     $Dir ACL read back: SYSTEM and Administrators only"
    }
    Step "DISABLE (not delete) the old rule '$OldRuleName' and record it in state.json, then read the disabled state back" {
        if ($old.Enabled -eq 'True') {
            Set-NetFirewallRule -Name $old.Name -Enabled False
            if ((Get-NetFirewallRule -Name $old.Name).Enabled -ne 'False') { throw "the old rule did not read back as disabled; run -Undo" }
            ([pscustomobject]@{ oldRuleName = $old.Name; disabledByUs = $true; wasEnabled = $true; at = (Get-Date -Format o) } | ConvertTo-Json) | Set-Content -Path $StateFile -Encoding ascii
        } else { Write-Host "       the old rule was already disabled; left as it is and not recorded as ours" }
    }
    if ($Live) {
        foreach ($r in $Rules) { Check "rule $($r.Name) present, Block, Outbound" { $g = Get-RuleFacts $r.Name; $g.Enabled -eq 'True' -and $g.Action -eq 'Block' -and $g.Direction -eq 'Outbound' } }
        Check "old rule '$OldRuleName' is disabled" { (Get-NetFirewallRule -Name $old.Name).Enabled -eq 'False' }
        Write-Host "`nNext (the lane, not this script): re-run the connect-only checks AS $AccountName and record them. Expected: external still refused; 10.77.x.x port 22 answers, other pool ports do not; loopback and the host's own addresses STILL answer (that is Part A)."
    }
}
if ($Live) {
    Write-Host "`n----- read-back -----"
    $results | Format-Table -AutoSize | Out-String | Write-Host
    if ($results | Where-Object { -not $_.Ok }) { Write-Host 'FAIL: at least one property did not read back as expected.'; exit 1 } else { Write-Host 'PASS: every property read back.' }
}
