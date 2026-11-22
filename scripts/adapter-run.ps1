# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Starts `spawn adapter` unattended (docs/ADAPTER-UNATTENDED.md, option A, ruled by the PM 2026-11-21). Run by the scheduled task
  `scripts/adapter-task.ps1` registers, as the logged-on user (the App key is DPAPI-protected for that user). It never elevates.

.DESCRIPTION
  1. `spawn adapter --config <Config> --check`. If it fails, one line goes to the log, the status file records the exit code,
     and this script exits 0 on purpose: a config or key error does not fix itself, so the task must NOT restart it.
  2. Otherwise `spawn adapter --config <Config> --status-file <StatusDir>\status.json` runs in the foreground (the adapter holds
     `status.json.lock`, so a second start fails at once). When it ends, this script adds `last_exit_code` and `exited_unix` to
     status.json and exits with the adapter's code: non-zero (a crash, or 3) lets Task Scheduler restart it (1 minute apart, 3 times).
  The status file holds names, numbers and times only. The PM's sweep reads its age and reports a stale tick; nothing is mailed.

.PARAMETER Config     The adapter configuration (TOML).
.PARAMETER Exe        spawn.exe. Default: spawn.exe in C:\Program Files\antspawn.
.PARAMETER StatusDir  Where status.json, its lock and adapter-run.log live. Default %LOCALAPPDATA%\antspawn\adapter (private to the user).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Config,
    [string]$Exe = 'C:\Program Files\antspawn\spawn.exe',
    [string]$StatusDir = (Join-Path $env:LOCALAPPDATA 'antspawn\adapter')
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $StatusDir | Out-Null
$status = Join-Path $StatusDir 'status.json'
$log = Join-Path $StatusDir 'adapter-run.log'

function Say([string]$what) { Add-Content -LiteralPath $log -Value ("{0:u} {1}" -f (Get-Date).ToUniversalTime(), $what) }
function Record([int]$code) {
    # add the wrapper's two fields to whatever the adapter last wrote (or start a file if it never ticked)
    $o = [ordered]@{}
    if (Test-Path -LiteralPath $status) {
        try { (Get-Content -Raw -LiteralPath $status | ConvertFrom-Json).PSObject.Properties | ForEach-Object { $o[$_.Name] = $_.Value } } catch { }
    }
    $o['last_exit_code'] = $code
    $o['exited_unix'] = [int64][DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $tmp = "$status.tmp"
    [IO.File]::WriteAllText($tmp, (($o | ConvertTo-Json) + "`n"), (New-Object Text.UTF8Encoding($false)))   # no BOM: the sweep's JSON reader must not choke
    Move-Item -LiteralPath $tmp -Destination $status -Force
}

if (-not (Test-Path -LiteralPath $Exe)) { Say "spawn.exe not found at $Exe"; Record 2; exit 0 }
$ErrorActionPreference = 'Continue'   # a native program's stderr is not a script error here
& $Exe adapter --config $Config --check 2>&1 | ForEach-Object { Say "check: $_" }
if ($LASTEXITCODE -ne 0) {
    Say "check failed (exit $LASTEXITCODE): not starting, not retrying"
    Record $LASTEXITCODE
    exit 0
}
Say 'check ok: starting the adapter'
& $Exe adapter --config $Config --status-file $status 2>&1 | ForEach-Object { Say "adapter: $_" }
$code = $LASTEXITCODE
Say "adapter ended with exit $code"
Record $code
# 0 clean; 2 could not start (another instance, key, config): no restart; anything else restarts
if ($code -eq 0 -or $code -eq 2) { exit 0 }
exit $code
