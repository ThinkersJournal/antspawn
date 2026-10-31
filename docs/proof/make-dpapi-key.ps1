# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Turns a GitHub App private-key PEM into the DPAPI file `spawn adapter` reads (DpapiKeyProvider). ONLY needed if
  %LOCALAPPDATA%\antspawn\github-app\antspawn-runner.key.dpapi is missing or the key was rotated. Not elevated: run it
  as cires, the account that will run the adapter. DRY RUN BY DEFAULT.
.DESCRIPTION
  Format (docs/ADAPTER-GITHUB.md): hex of a DPAPI user-scope blob, as `ConvertFrom-SecureString` writes it, whose
  plaintext is the PEM. Only the writing Windows account can decrypt it. The PEM is never printed. With -Apply the
  script writes the file, locks it to the current user, reads it back (decrypts and checks it begins with a PEM
  header, printing only PASS/FAIL), and deletes the PEM you gave it (a delete on an SSD is not a secure wipe: if the
  PEM sat in Downloads, revoke that key in the App settings once the new one works).
.PARAMETER Pem    Path of the downloaded .pem (App settings > Private keys > Generate a private key).
.PARAMETER Out    Default %LOCALAPPDATA%\antspawn\github-app\antspawn-runner.key.dpapi
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Pem,
    [string]$Out = (Join-Path $env:LOCALAPPDATA 'antspawn\github-app\antspawn-runner.key.dpapi'),
    [switch]$Apply
)
$ErrorActionPreference = 'Stop'
try {
    if (-not (Test-Path -LiteralPath $Pem -PathType Leaf)) { throw "no such file: $Pem" }
    $head = (Get-Content -LiteralPath $Pem -TotalCount 1)
    if ($head -notmatch '^-----BEGIN (RSA )?PRIVATE KEY-----$') { throw "$Pem does not start with a PEM private-key header" }
    $dir = Split-Path $Out -Parent
    if (-not $Apply) {
        Write-Host "WOULD  read the PEM at $Pem (header ok, contents not shown)"
        Write-Host "WOULD  write $Out as hex of a DPAPI user-scope blob for $env:USERDOMAIN\$env:USERNAME"
        Write-Host "WOULD  restrict it to $env:USERNAME + SYSTEM, read it back, delete $Pem"
        if (Test-Path -LiteralPath $Out) { Write-Host "NOTE   $Out exists and would be REPLACED" }
        Write-Host 'Dry run: nothing changed. Re-run with -Apply.'
        exit 0
    }
    New-Item -ItemType Directory -Force $dir | Out-Null
    $secure = ConvertTo-SecureString (Get-Content -LiteralPath $Pem -Raw) -AsPlainText -Force
    ConvertFrom-SecureString $secure | Set-Content -LiteralPath $Out -Encoding ascii
    & icacls.exe $Out /inheritance:r /grant:r "${env:USERNAME}:(R,W,D)" 'SYSTEM:(R)' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls failed ($LASTEXITCODE)" }
    $back = ConvertTo-SecureString ((Get-Content -LiteralPath $Out -Raw).Trim())
    $plain = [Runtime.InteropServices.Marshal]::PtrToStringUni([Runtime.InteropServices.Marshal]::SecureStringToGlobalAllocUnicode($back))
    $ok = $plain -match '^-----BEGIN (RSA )?PRIVATE KEY-----'
    $plain = $null
    Write-Host ("{0,-5} the file decrypts as this user and holds a PEM private key" -f $(if ($ok) { 'PASS' } else { 'FAIL' }))
    if (-not $ok) { throw 'read-back failed; the PEM was NOT deleted' }
    Remove-Item -LiteralPath $Pem -Force
    Write-Host "Done. Deleted $Pem. Now: cargo run --example github_proof -- probe docs/proof/adapter.toml"
} catch {
    Write-Host "FAILED: $($_.Exception.Message)"
    exit 1
}
