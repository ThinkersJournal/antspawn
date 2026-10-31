# SPDX-License-Identifier: MIT OR Apache-2.0
# Reads a scheduled task's security descriptor NUMERICALLY. Windows prints the mask it set as GRGX (generic read +
# execute) back as 0x1200a9, and Task Scheduler adds entries of its own (the run-as account, inherited BA/SY), so
# matching the SDDL text for 'GRGX' gives a false FAIL (seen on the first real -Apply, 2026-10-31).
# Dot-sourced by runner-account-setup.ps1 and by tests/task_sd_check.rs.

# Returns the problems found (empty = the descriptor is as intended):
#   launcher  : allowed exactly 0x1200a9 (read + execute, SYNCHRONIZE, READ_CONTROL): no write, delete or change-permission bit
#   run-as    : read only (no bit beyond FILE_GENERIC_READ 0x120089); may be absent
#   BA and SY : allowed, any mask (administrators and SYSTEM can already do anything)
#   anyone else, or any non-Allow entry: a problem
function Get-TaskSdProblems {
    param([Parameter(Mandatory)][string]$Sddl, [Parameter(Mandatory)][string]$LauncherSid, [Parameter(Mandatory)][string]$RunAsSid)
    $problems = [System.Collections.Generic.List[string]]::new()
    $sd = New-Object Security.AccessControl.RawSecurityDescriptor($Sddl)
    if ($null -eq $sd.DiscretionaryAcl) { return @('the descriptor has no DACL (everyone allowed)') }
    $launcher = 0
    $runAs = 0
    foreach ($ace in $sd.DiscretionaryAcl) {
        $sid = $ace.SecurityIdentifier.Value
        $mask = [int]$ace.AccessMask
        if ($ace.AceType -ne [Security.AccessControl.AceType]::AccessAllowed) {
            $problems.Add("non-allow entry ($($ace.AceType)) for $sid"); continue
        }
        if ($sid -eq $LauncherSid) { $launcher = $launcher -bor $mask }
        elseif ($sid -eq $RunAsSid) { $runAs = $runAs -bor $mask }
        elseif ($sid -eq 'S-1-5-32-544' -or $sid -eq 'S-1-5-18') { }
        else { $problems.Add("unexpected principal $sid has mask 0x{0:x}" -f $mask) }
    }
    if ($launcher -ne 0x1200a9) { $problems.Add(('launcher {0} has mask 0x{1:x}, want exactly 0x1200a9' -f $LauncherSid, $launcher)) }
    if (($runAs -band (-bnot 0x120089)) -ne 0) { $problems.Add(('run-as account has mask 0x{0:x}, want read only (within 0x120089)' -f $runAs)) }
    return $problems.ToArray()
}
