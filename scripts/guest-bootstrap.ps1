# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Runs INSIDE a runner VM, once per boot, as SYSTEM (a startup scheduled task baked into the base image): waits for the host to
  put this VM's password on the VM bus, sets it as the local Administrator's password, and exits. ROADMAP row 40, PM ruling
  2026-11-26: no long-lived guest credential.
.DESCRIPTION
  The host (src/adapter/vm_backend.rs script_provision) adds a Hyper-V data-exchange item named antspawn-guest-pw; the guest sees it
  under HKLM:\SOFTWARE\Microsoft\Virtual Machine\External. The base image's Administrator account is left with a random
  password nobody kept (the bake's last step), so before this script runs there is no credential at all. The value is held only
  in a SecureString and a char array that is cleared; nothing is printed, logged, written to a file, put in an environment
  variable or passed on a command line. The host removes the item once PowerShell Direct has answered with the new password.
  NOT RUN yet: it needs a rebake of the base image (docs/RUNNER-VM-PATH.md) and the host side measured over a real VM.
#>
$ErrorActionPreference = 'Stop'
$name = 'antspawn-guest-pw'
$key = 'HKLM:\SOFTWARE\Microsoft\Virtual Machine\External'
$end = (Get-Date).AddMinutes(10)
$value = $null
while (-not $value) {
    $value = (Get-ItemProperty -LiteralPath $key -Name $name -ErrorAction SilentlyContinue).$name
    if ($value) { break }
    if ((Get-Date) -gt $end) { exit 3 }
    Start-Sleep -Seconds 1
}
$chars = $value.ToCharArray()
$value = $null
$ss = New-Object System.Security.SecureString
foreach ($c in $chars) { $ss.AppendChar($c) }
$ss.MakeReadOnly()
[Array]::Clear($chars, 0, $chars.Length)
Set-LocalUser -Name 'Administrator' -Password $ss
Enable-LocalUser -Name 'Administrator'
$ss.Dispose()
