# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Windows Server 2025 Core pilot (docs/RUNNER-VM-PATH.md, "Windows Server 2025 Core as the guest"): ONE throwaway Hyper-V
  VM, no network at all, driven over PowerShell Direct (the VM bus). Run as the interactive user who is in
  Hyper-V Administrators; it never elevates and never creates accounts.

.DESCRIPTION
  DRY-RUN BY DEFAULT: without -Apply a step prints what it would do and changes nothing. A step that needs the image reads
  it only when -Apply is given. The guest has NO network: an Internal switch with no host address, no NAT, no DNS. Everything
  reaches the guest by Copy-Item -ToSession (PowerShell Direct). Nothing here contacts GitHub.

  Steps (run in this order; each is safe to re-run):
    Plan      print the plan and the host-side facts it relies on (the default)
    Verify    hash the image and compare with -Sha256 (a hash is printed even when none is given: nothing is "published" to compare to
              unless the Evaluation Center showed one)
    Create    internal switch, differencing child of the read-only base, one VM (generation from the image's format)
    Boot      Start-VM and time it to the first heartbeat
    Licence   DISM /GetEula inside the guest, copy the text out, print it (the four sentences are read from it by a human)
    Aocl      copy AOCL and the five Intel runtime DLLs into the guest and LoadLibrary scalapack.dll there (no MSVC needed)
    Destroy   stop and remove the VM, delete the child and the switch; the base is kept

  The guest Administrator password is read from the environment variable ANTSPAWN_PILOT_PW (set by the person running this;
  it is never printed or written). The first boot of a Server evaluation image asks for an Administrator password at the
  console: that one prompt is interactive (vmconnect) unless the image already has one.

.PARAMETER Step    Plan | Verify | Create | Boot | Licence | Aocl | Destroy
.PARAMETER Apply   Make the changes. Without it nothing is changed.
.PARAMETER Image   The evaluation image (.vhd or .vhdx) in the new folder CireSnave downloads into. Never under a scripts folder.
.PARAMETER Sha256  The published checksum, if the Evaluation Center showed one.
.PARAMETER Work    Folder for the child disk and results. Default C:\vmimages\pilot.
#>
[CmdletBinding()]
param(
    [ValidateSet('Plan', 'Verify', 'Create', 'Boot', 'Licence', 'Aocl', 'Destroy')][string]$Step = 'Plan',
    [switch]$Apply,
    [string]$Image = 'C:\vmimages\WindowsServer2025-Core-eval.vhd',
    [string]$Sha256 = '',
    [string]$Work = 'C:\vmimages\pilot'
)
$ErrorActionPreference = 'Stop'
$VmName = 'antspawn-pilot-core'
$SwitchName = 'antspawn-pilot-sw'
$Child = Join-Path $Work 'child.vhdx'
$Mode = if ($Apply) { 'APPLY' } else { 'DRY RUN (nothing is changed)' }
"vm-pilot: step=$Step mode=$Mode"

function Say([string]$what) { "  would: $what" }
function Cred {
    $pw = $env:ANTSPAWN_PILOT_PW
    if (-not $pw) { throw 'set ANTSPAWN_PILOT_PW to the guest Administrator password first (it is never printed)' }
    New-Object System.Management.Automation.PSCredential('Administrator', (ConvertTo-SecureString $pw -AsPlainText -Force))
}
function Oneapi {
    $o = Join-Path ${env:ProgramFiles(x86)} 'Intel\oneAPI'
    foreach ($f in 'impi.dll', 'libifcoremd.dll', 'libmmd.dll', 'svml_dispmd.dll', 'libiomp5md.dll') {
        # oneAPI's umbrella folder (2026.1in) holds 0-byte symlinks to the real DLLs: take the first real file.
        $h = Get-ChildItem -LiteralPath $o -Filter $f -Recurse -ErrorAction SilentlyContinue | Where-Object { -not $_.LinkType -and $_.Length -gt 0 -and $_.FullName -notmatch "bin32|ia32|[\/]x86[\/]|win32" } | Select-Object -First 1
        [pscustomobject]@{ Name = $f; Path = if ($h) { $h.FullName } else { '(not found)' }; Bytes = if ($h) { $h.Length } else { 0 } }
    }
}

switch ($Step) {
    'Plan' {
        'The guest has no network. Steps: Verify, Create, Boot, Licence, Aocl, Destroy (see -?).'
        "Image            : $Image  (present: $(Test-Path -LiteralPath $Image))"
        "Work folder      : $Work"
        "VM / switch      : $VmName / $SwitchName (Internal, no host address)"
        $a = Join-Path $env:ProgramFiles 'AMD\AOCL-Windows'
        "AOCL on the host : $a (present: $(Test-Path -LiteralPath $a))"
        'Intel runtime DLLs on the host (bytes shown; a 0 means a stub or link, not a usable copy):'
        Oneapi | Format-Table -AutoSize | Out-String
        "Hyper-V Administrator (can Get-VM): $([bool](Get-Command Get-VM -ErrorAction SilentlyContinue))"
        'Not done by this script: the Build Tools, Git, the pinned runner and cargo steps (a later step, after the Licence and Aocl results).'
    }
    'Verify' {
        if (-not $Apply) { Say "Get-FileHash -Algorithm SHA256 '$Image' and compare with '$Sha256' (empty = print only)"; break }
        $h = (Get-FileHash -Algorithm SHA256 -LiteralPath $Image).Hash
        "sha256 $h"
        if ($Sha256) { if ($h -ieq $Sha256) { 'MATCH the published checksum' } else { throw "MISMATCH: published $Sha256" } }
        else { 'no published checksum was given: nothing to compare against' }
    }
    'Create' {
        if (-not $Apply) {
            Say "mark '$Image' read-only (the base is kept)"
            Say "New-VMSwitch -Name $SwitchName -SwitchType Internal (no host IP, no NAT)"
            Say "New-VHD -Differencing -ParentPath '$Image' -Path '$Child'"
            Say "New-VM -Name $VmName -Generation <1 for a .vhd, 2 for a .vhdx> -VHDPath '$Child' -SwitchName $SwitchName -MemoryStartupBytes 4GB; Set-VM -CheckpointType Disabled; no integration services removed"
            break
        }
        if (-not (Test-Path -LiteralPath $Work)) { New-Item -ItemType Directory -Path $Work | Out-Null }
        Set-ItemProperty -LiteralPath $Image -Name IsReadOnly -Value $true
        if (-not (Get-VMSwitch -Name $SwitchName -ErrorAction SilentlyContinue)) { New-VMSwitch -Name $SwitchName -SwitchType Internal | Out-Null }
        New-VHD -Differencing -ParentPath $Image -Path $Child | Out-Null
        $gen = if ((Get-VHD -Path $Image).VhdFormat -eq 'VHDX') { 2 } else { 1 }
        New-VM -Name $VmName -Generation $gen -VHDPath $Child -SwitchName $SwitchName -MemoryStartupBytes 4GB | Out-Null
        Set-VM -Name $VmName -CheckpointType Disabled -ProcessorCount 2
        "created $VmName generation $gen on $SwitchName; base size $((Get-Item -LiteralPath $Image).Length) bytes"
    }
    'Boot' {
        if (-not $Apply) { Say "Start-VM $VmName; poll the Heartbeat integration service until OK; print seconds"; break }
        $t = [Diagnostics.Stopwatch]::StartNew()
        Start-VM -Name $VmName
        while ((Get-VMIntegrationService -VMName $VmName -Name Heartbeat).PrimaryStatusDescription -ne 'OK') {
            if ($t.Elapsed.TotalSeconds -gt 900) { throw 'no heartbeat in 15 minutes (the first boot may be waiting for the Administrator password at the console)' }
            Start-Sleep -Seconds 2
        }
        "first heartbeat after $([math]::Round($t.Elapsed.TotalSeconds,1)) s (one sample, this host, now)"
    }
    'Licence' {
        if (-not $Apply) {
            Say "PowerShell Direct into ${VmName}: DISM /Online /Get-CurrentEdition, /Get-TargetEditions"
            Say "DISM /Online /Set-Edition:<first target edition> /GetEula:C:\license.rtf  (writes the text; it does NOT convert or activate)"
            Say "Copy-Item -FromSession C:\license.rtf -> $Work\license.rtf and print it as plain text for a human to quote"
            break
        }
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock {
                DISM /Online /Get-CurrentEdition
                $t = (DISM /Online /Get-TargetEditions | Select-String 'Target Edition : (\S+)' | Select-Object -First 1).Matches.Groups[1].Value
                "target edition: $t"
                DISM /Online /Set-Edition:$t /GetEula:C:\license.rtf
            }
            Copy-Item -FromSession $s -Path C:\license.rtf -Destination (Join-Path $Work 'license.rtf')
        } finally { Remove-PSSession $s }
        "saved $Work\license.rtf"
    }
    'Aocl' {
        if (-not $Apply) {
            Say "copy the host's AOCL tree ($env:ProgramFiles\AMD\AOCL-Windows) and the five Intel runtime DLLs into $VmName over PowerShell Direct"
            Say "in the guest: LoadLibrary(scalapack.dll) with the same PATH the aocl workflow builds; print the Win32 error and the missing dependency if it fails"
            Say 'the cargo test of aocl-scalapack needs MSVC (Build Tools) in the guest: a later step'
            break
        }
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Copy-Item -ToSession $s -Path (Join-Path $env:ProgramFiles 'AMD\AOCL-Windows') -Destination 'C:\AOCL-Windows' -Recurse -Force
            Invoke-Command -Session $s -ScriptBlock { New-Item -ItemType Directory -Path C:\oneapi -Force | Out-Null }
            foreach ($d in Oneapi) { if ($d.Bytes -gt 0) { Copy-Item -ToSession $s -Path $d.Path -Destination C:\oneapi -Force } else { "skipped $($d.Name): not a usable file on the host" } }
            Invoke-Command -Session $s -ScriptBlock {
                $dirs = 'C:\AOCL-Windows\amd-scalapack\lib\LP64\shared', 'C:\AOCL-Windows\amd-blis\lib\LP64', 'C:\AOCL-Windows\amd-libflame\lib\LP64', 'C:\AOCL-Windows\amd-utils\lib', 'C:\oneapi'
                $env:PATH = ($dirs -join ';') + ';' + $env:PATH
                Add-Type -MemberDefinition '[DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern System.IntPtr LoadLibraryW(string f);' -Name K -Namespace W
                $h = [W.K]::LoadLibraryW('C:\AOCL-Windows\amd-scalapack\lib\LP64\shared\scalapack.dll')
                if ($h -eq [IntPtr]::Zero) { "LoadLibrary FAILED, Win32 error $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" } else { 'LoadLibrary scalapack.dll OK' }
            }
        } finally { Remove-PSSession $s }
    }
    'Destroy' {
        if (-not $Apply) { Say "Stop-VM -TurnOff $VmName; Remove-VM; delete '$Child'; Remove-VMSwitch $SwitchName; keep '$Image'"; break }
        if (Get-VM -Name $VmName -ErrorAction SilentlyContinue) { Stop-VM -Name $VmName -TurnOff -Force -ErrorAction SilentlyContinue; Remove-VM -Name $VmName -Force }
        if (Test-Path -LiteralPath $Child) { Remove-Item -LiteralPath $Child -Force }
        if (Get-VMSwitch -Name $SwitchName -ErrorAction SilentlyContinue) { Remove-VMSwitch -Name $SwitchName -Force }
        'destroyed the VM, child disk and switch; the base image is kept'
    }
}
