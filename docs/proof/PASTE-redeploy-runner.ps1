# SPDX-License-Identifier: MIT OR Apache-2.0
# Redeploy spawn-runner.exe into C:\Program Files\antspawn. ELEVATED (the folder is Administrators-write only).
# Changes no ACL, no account, no task: it swaps one file and reads it back. See docs/REDEPLOY-RUNNER.md.
#   & .\PASTE-redeploy-runner.ps1 -Source C:\Users\cires\antspawn-staging\runner-<sha8>\spawn-runner.exe
param([Parameter(Mandatory)][string]$Source)
$ErrorActionPreference = 'Stop'
$Bin = 'C:\Program Files\antspawn'; $Dest = Join-Path $Bin 'spawn-runner.exe'
$id = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $id.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'run this elevated' }
$want = ((Get-Content "$Source.sha256" -TotalCount 1) -split '\s+')[0].ToUpper()
$got = (Get-FileHash $Source -Algorithm SHA256).Hash
if ($got -ne $want) { throw "source sha256 $got does not match its .sha256 file ($want); not deploying" }
if (Get-Process spawn-runner -ErrorAction SilentlyContinue) { throw 'a spawn-runner is running (a job or session is live); wait or end it' }
$old = (Get-FileHash $Dest -Algorithm SHA256).Hash
Copy-Item $Dest "$Dest.bak-pre-$($got.Substring(0,8))" -Force
Copy-Item $Source "$Dest.new" -Force
if ((Get-FileHash "$Dest.new" -Algorithm SHA256).Hash -ne $got) { Remove-Item "$Dest.new"; throw 'copy did not verify' }
Move-Item "$Dest.new" $Dest -Force
$now = (Get-FileHash $Dest -Algorithm SHA256).Hash
if ($now -ne $got) { throw "after the swap $Dest is $now, not $got" }
"before $old`nafter  $now  ($((Get-Item $Dest).Length) bytes)"
& icacls.exe $Dest
