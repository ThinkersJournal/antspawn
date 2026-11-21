# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Registers (or removes) the scheduled task that starts the adapter at logon (docs/ADAPTER-UNATTENDED.md, option A, ruled by the
  PM 2026-11-21): task `AntSpawnAdapter`, for the CURRENT user only, "run only when the user is logged on", limited (not elevated),
  no stored password. DRY RUN BY DEFAULT: without -Apply it changes nothing and prints what it would do. -Apply registers and
  reads each property back (PASS/FAIL). -Undo -Apply unregisters and reads it back absent. It never starts the task.

.DESCRIPTION
  The task runs `powershell.exe -NoProfile -ExecutionPolicy Bypass -File <RunScript> -Config <Config> [-Exe ...]` at logon of the
  current user, restarts on failure 3 times 1 minute apart, has no time limit, and starts no second instance. The registration
  needs no elevation for a task of your own that runs only while you are logged on (to be confirmed by this script's read-back:
  if Register-ScheduledTask is refused, it says so and changes nothing; that is then an elevation item for the PM).
  Registering it means the adapter starts at your next logon and polls GitHub: do that only when the PM says live running is allowed.

.PARAMETER Config     The adapter configuration (TOML), absolute path.
.PARAMETER RunScript  scripts\adapter-run.ps1, absolute path. Default: beside this script.
.PARAMETER Exe        spawn.exe passed through to the run script (its default is C:\Program Files\antspawn\spawn.exe).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Config,
    [string]$RunScript = '',
    [string]$Exe = '',
    [switch]$Apply,
    [switch]$Undo
)
$ErrorActionPreference = 'Stop'
if ($Apply -and -not $Undo -and -not [IO.Path]::IsPathRooted($Config)) { throw 'give -Config as an absolute path' }
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $RunScript) { $RunScript = Join-Path $here 'adapter-run.ps1' }
$Name = 'AntSpawnAdapter'
$user = "$env:USERDOMAIN\$env:USERNAME"
$mode = if ($Apply) { 'APPLY' } else { 'DRY RUN (nothing is changed)' }
"adapter-task: $(if ($Undo) { 'undo' } else { 'register' }) $Name for $user, mode=$mode"

$argLine = "-NoProfile -ExecutionPolicy Bypass -File `"$RunScript`" -Config `"$Config`""
if ($Exe) { $argLine += " -Exe `"$Exe`"" }

if ($Undo) {
    if (-not $Apply) { "  would: Unregister-ScheduledTask -TaskName $Name -Confirm:`$false, then read it back absent"; return }
    if (Get-ScheduledTask -TaskName $Name -ErrorAction SilentlyContinue) { Unregister-ScheduledTask -TaskName $Name -Confirm:$false }
    $gone = -not (Get-ScheduledTask -TaskName $Name -ErrorAction SilentlyContinue)
    "{0}: task absent" -f $(if ($gone) { 'PASS' } else { 'FAIL' })
    if (-not $gone) { exit 1 }
    return
}

"  action   : powershell.exe $argLine"
'  trigger  : at logon of the current user'
'  principal: the current user, interactive logon only, limited (not elevated), no stored password'
'  settings : restart on failure 3 times 1 minute apart; no time limit; do not start a new instance; allowed on battery'
if (-not $Apply) {
    '  would: Register-ScheduledTask ... then read back the action, trigger, run level, logon type and settings'
    "  the exact line to register it for real:  powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$($MyInvocation.MyCommand.Path)`" -Config `"$Config`" -Apply"
    return
}
if (-not (Test-Path -LiteralPath $RunScript)) { throw "run script not found: $RunScript" }
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $argLine
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
try {
    Register-ScheduledTask -TaskName $Name -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
} catch {
    "REFUSED: $($_.Exception.Message) (nothing was changed; if this is an access error it is an elevation item for the PM)"
    exit 1
}
$t = Get-ScheduledTask -TaskName $Name
$fail = 0
function Check([string]$what, [bool]$ok) { if (-not $ok) { $script:fail++ }; '{0}: {1}' -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $what }
Check 'action runs powershell.exe with the run script' ($t.Actions[0].Execute -eq 'powershell.exe' -and $t.Actions[0].Arguments -like "*$RunScript*")
Check 'trigger is at logon' ($t.Triggers[0].CimClass.CimClassName -eq 'MSFT_TaskLogonTrigger')
Check 'run level is Limited' ($t.Principal.RunLevel -eq 'Limited')
Check 'logon type is Interactive (no stored password)' ($t.Principal.LogonType -eq 'Interactive')
Check 'restart count 3' ($t.Settings.RestartCount -eq 3)
Check 'one instance only' ($t.Settings.MultipleInstances -eq 'IgnoreNew')
Check 'state is not Running (this script never starts it)' ($t.State -ne 'Running')
if ($fail) { exit 1 }
