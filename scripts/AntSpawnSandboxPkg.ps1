# SPDX-License-Identifier: MIT OR Apache-2.0
# Registers the Windows Sandbox Store package (wsb.exe) for ONE local account, and reads the result back.
# Dot-sourced by runner-account-setup.ps1 and account-setup.ps1 (elevated, -Apply only).
#
# WHY this exists (PM measurement 2026-11-02, files in C:\Projects\.lane-state\sandbox\): the optional feature
# Containers-DisposableClientVM is enabled, but package MicrosoftWindows.WindowsSandbox is registered only for the
# interactive user and is NOT provisioned (Add-AppxProvisionedPackage fails: no .main package file), so a restricted
# runner account has no wsb.exe. Registering as the account itself worked on a throwaway account:
#   Add-AppxPackage -RegisterByFamilyName -MainPackage MicrosoftWindows.WindowsSandbox_cw5n1h2txyewy
# run through Start-Process -Credential -LoadUserProfile. A scheduled task (S4U) for the account gets Access denied,
# so this needs the account's password: callers hold it in memory only (it is discarded right after the task is registered).
#
# What it does NOT do: it does not make `wsb list` work for the account. As ghr-style accounts it printed
# Status(StatusCode="Cancelled") (UNEXPLAINED). Untested hypothesis, not a finding: the interactive user, for whom
# `wsb list` exits 0, is a member of BUILTIN\Hyper-V Administrators (S-1-5-32-578), which a runner account must not be in.

$script:SandboxPackageName = 'MicrosoftWindows.WindowsSandbox'
$script:SandboxFamilyName = 'MicrosoftWindows.WindowsSandbox_cw5n1h2txyewy'

# Runs the registration AS the account (needs its password) and returns what the child printed.
# The child's output goes to a file under C:\Users\Public (the account can write there), is read, then deleted. It holds no secret.
function Register-SandboxPackageAs {
    param([Parameter(Mandatory)][string]$AccountName, [Parameter(Mandatory)][string]$Plain, [int]$TimeoutS = 180)
    $log = Join-Path 'C:\Users\Public' ("antspawn-wsb-" + [guid]::NewGuid().ToString('N') + '.txt')
    $sec = ConvertTo-SecureString $Plain -AsPlainText -Force
    $cred = New-Object System.Management.Automation.PSCredential("$env:COMPUTERNAME\$AccountName", $sec)
    $child = @'
try { Add-AppxPackage -RegisterByFamilyName -MainPackage __FAMILY__ -ErrorAction Stop; 'REGISTER=ok' } catch { 'REGISTER=FAIL ' + $_.Exception.Message }
$w = (Get-Command wsb.exe -ErrorAction SilentlyContinue).Source
'WSB-EXE=' + $w
if ($w) { 'WSB-LIST-BEGIN'; (& $w list 2>&1 | Out-String); 'WSB-LIST-EXIT=' + $LASTEXITCODE; 'WSB-LIST-END' }
'@
    $child = $child.Replace('__FAMILY__', $script:SandboxFamilyName)
    $wrapped = "& { $child } *> '$log'"
    $timedOut = $false
    try {
        $p = Start-Process powershell.exe -Credential $cred -LoadUserProfile -WorkingDirectory 'C:\Users\Public' `
            -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', $wrapped -WindowStyle Hidden -PassThru
        if (-not $p.WaitForExit($TimeoutS * 1000)) { $timedOut = $true; try { $p.Kill() } catch {} }
    } finally { $sec.Dispose() }
    $raw = if (Test-Path $log) { Get-Content $log -Raw } else { '' }
    Remove-Item $log -ErrorAction SilentlyContinue
    [pscustomobject]@{
        Registered = ($raw -match '(?m)^REGISTER=ok\s*$')
        WsbExe     = if ($raw -match '(?m)^WSB-EXE=(.+?)\s*$') { $Matches[1] } else { '' }
        ListExit   = if ($raw -match '(?m)^WSB-LIST-EXIT=(\d+)') { [int]$Matches[1] } else { $null }
        Raw        = $raw
        TimedOut   = $timedOut
    }
}

# InstallState of the Sandbox package for the account's SID, as Windows reports it for all users ('' when absent).
function Get-SandboxPackageState {
    param([Parameter(Mandatory)][string]$Sid)
    foreach ($pkg in @(Get-AppxPackage -AllUsers -Name $script:SandboxPackageName -ErrorAction SilentlyContinue)) {
        foreach ($ui in @($pkg.PackageUserInformation)) {
            $usid = if ($ui.UserSecurityId.PSObject.Properties['Sid']) { $ui.UserSecurityId.Sid } else { "$($ui.UserSecurityId)" }
            if ($usid -eq $Sid) { return "$($ui.InstallState)" }
        }
    }
    ''
}

# Where wsb.exe should appear: under the account's own profile (the profile exists once -LoadUserProfile has run).
function Get-SandboxExePath {
    param([Parameter(Mandatory)][string]$Sid)
    $prof = Get-CimInstance Win32_UserProfile -Filter "SID='$Sid'" -ErrorAction SilentlyContinue
    if ($prof -and $prof.LocalPath) { Join-Path $prof.LocalPath 'AppData\Local\Microsoft\WindowsApps\wsb.exe' } else { '' }
}
