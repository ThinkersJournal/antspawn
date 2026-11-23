# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Stores the runner-VM guest Administrator password as a DPAPI file the adapter reads for PowerShell Direct
  (ROADMAP row 40; CireSnave ruled board 488 "A"). Not elevated: run it as cires, the account that runs the adapter.
  DRY RUN BY DEFAULT.
.DESCRIPTION
  The baked image (core-vendor-base.vhdx) has ONE fixed Administrator password, so it is in every child VM and rotating
  it means rebaking. The file is hex of a DPAPI user-scope blob as ConvertFrom-SecureString writes it (the same form as
  the GitHub App key), so only the writing Windows account decrypts it. The password comes from a hidden prompt: it is
  never an argument, never echoed, never logged, never written anywhere but the DPAPI file. The adapter builds the
  PSCredential from that file inside the PowerShell it runs and never writes the value out.
    (no switch)  print what would happen; read nothing, write nothing
    -Apply       prompt (hidden), write the file, lock it to this user + SYSTEM, read it back (PASS/FAIL only)
    -Undo        dry run of the removal; with -Apply too, delete the file
.PARAMETER Out  Default %LOCALAPPDATA%\antspawn\guest-credential.dpapi
#>
[CmdletBinding()]
param(
    [string]$Out = (Join-Path $env:LOCALAPPDATA 'antspawn\guest-credential.dpapi'),
    [switch]$Apply,
    [switch]$Undo
)
$ErrorActionPreference = 'Stop'
try {
    if ($Undo) {
        if (-not (Test-Path -LiteralPath $Out)) { Write-Host "Nothing to remove: $Out does not exist."; exit 0 }
        if (-not $Apply) { Write-Host "WOULD  delete $Out`nDry run: nothing changed. Re-run with -Undo -Apply."; exit 0 }
        Remove-Item -LiteralPath $Out -Force
        Write-Host "Deleted $Out. Rotating the guest password means rebaking the image."
        exit 0
    }
    if (-not $Apply) {
        Write-Host "WOULD  ask for the guest Administrator password at a hidden prompt (nothing is echoed)"
        Write-Host "WOULD  write $Out as hex of a DPAPI user-scope blob for $env:USERDOMAIN\$env:USERNAME"
        Write-Host "WOULD  restrict it to $env:USERNAME + SYSTEM, then read it back and print PASS or FAIL only"
        if (Test-Path -LiteralPath $Out) { Write-Host "NOTE   $Out exists and would be REPLACED" }
        Write-Host 'Dry run: nothing changed. Re-run with -Apply.'
        exit 0
    }
    $secure = Read-Host -AsSecureString 'Guest Administrator password (hidden)'
    if ($secure.Length -eq 0) { throw 'empty password: nothing written' }
    New-Item -ItemType Directory -Force (Split-Path $Out -Parent) | Out-Null
    ConvertFrom-SecureString $secure | Set-Content -LiteralPath $Out -Encoding ascii
    & icacls.exe $Out /inheritance:r /grant:r "${env:USERNAME}:(R,W,D)" 'SYSTEM:(R)' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls failed ($LASTEXITCODE)" }
    $back = ConvertTo-SecureString ((Get-Content -LiteralPath $Out -Raw).Trim())
    $ok = ($back.Length -eq $secure.Length)
    Write-Host ("{0,-5} the file decrypts as this user and holds a password of the length you typed" -f $(if ($ok) { 'PASS' } else { 'FAIL' }))
    if (-not $ok) { throw 'read-back failed' }
} catch {
    Write-Host "FAILED: $($_.Exception.Message)"
    exit 1
}
