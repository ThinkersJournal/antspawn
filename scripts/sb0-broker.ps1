# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  SB0 prototype (docs/SANDBOX-BROKER.md, Track 2): a Windows Sandbox broker run in the INTERACTIVE session of
  the user who runs this (no elevation). It serves one pipe whose DACL names this user and the runner account
  only; on a client's `start` line it runs `wsb start`, waits until `wsb exec` works inside, replies with the
  timings, and stops the Sandbox when the client disconnects. A MEASUREMENT HARNESS, not the broker: it has
  no validation and no caps; the template is a literal and the request carries no data.
#>
param([string]$RunnerUser = 'antspawn-run', [int]$Minutes = 20, [string]$Log = (Join-Path $env:TEMP 'sb0-broker.log'))
$ErrorActionPreference = 'Stop'
trap { Add-Content -Path $Log -Value ("{0:o} FATAL {1}" -f (Get-Date), $_) -Encoding utf8; break }
function Rec([string]$s) { Add-Content -Path $Log -Value ("{0:o} {1}" -f (Get-Date), $s) -Encoding utf8 }
$me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$runner = ([Security.Principal.NTAccount]$RunnerUser).Translate([Security.Principal.SecurityIdentifier]).Value
function New-Pipe {
    $ps = New-Object System.IO.Pipes.PipeSecurity
    $ps.SetSecurityDescriptorSddlForm("D:P(A;;FA;;;$me)(A;;0x12019f;;;$runner)")
    # Windows PowerShell 5.1 (.NET Framework) has the PipeSecurity constructor; PowerShell 7 (.NET) needs NamedPipeServerStreamAcl.
    if ('System.IO.Pipes.NamedPipeServerStreamAcl' -as [type]) {
        [System.IO.Pipes.NamedPipeServerStreamAcl]::Create('antspawn-sb0', [System.IO.Pipes.PipeDirection]::InOut, 1,
            [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::Asynchronous, 4096, 4096, $ps)
    } else {
        New-Object System.IO.Pipes.NamedPipeServerStream('antspawn-sb0', [System.IO.Pipes.PipeDirection]::InOut, 1,
            [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::Asynchronous, 4096, 4096, $ps)
    }
}
Rec "START as $([Security.Principal.WindowsIdentity]::GetCurrent().Name), pipe antspawn-sb0, runner sid $runner"
$deadline = (Get-Date).AddMinutes($Minutes)
$pipe = New-Pipe; $wait = $pipe.WaitForConnectionAsync()
while ((Get-Date) -lt $deadline) {
    if (-not $wait.IsCompleted) { Start-Sleep -Milliseconds 200; continue }
    $id = $null
    try {
        $r = New-Object System.IO.StreamReader($pipe); $w = New-Object System.IO.StreamWriter($pipe); $w.AutoFlush = $true
        $line = $r.ReadLine()
        $who = $null; $pipe.RunAsClient([System.IO.Pipes.PipeStreamImpersonationWorker]{ $script:who = [Security.Principal.WindowsIdentity]::GetCurrent().Name })
        Rec "client identity=$who said=$line"
        if ($line -eq 'start') {
            $sw = [Diagnostics.Stopwatch]::StartNew()
            $o = (& wsb start --raw 2>&1 | Out-String).Trim()
            $startMs = $sw.ElapsedMilliseconds
            Rec "wsb start (${startMs} ms): $o"
            $id = ($o | ConvertFrom-Json -ErrorAction SilentlyContinue).Id
            if (-not $id -and $o -match '([0-9a-fA-F]{8}-[0-9a-fA-F-]{27})') { $id = $Matches[1] }
            $readyMs = -1
            if ($id) {
                for ($i = 0; $i -lt 120 -and $readyMs -lt 0; $i++) {
                    $e = (& wsb exec --id $id -c 'cmd.exe /c echo ready' -r System 2>&1 | Out-String).Trim()
                    if ($LASTEXITCODE -eq 0) { $readyMs = $sw.ElapsedMilliseconds } else { Start-Sleep -Milliseconds 500 }
                }
                Rec "exec ready after ${readyMs} ms; last exec output: $e"
            }
            $w.WriteLine((@{ id = $id; start_ms = $startMs; ready_ms = $readyMs; broker_user = $env:USERNAME; client = $who } | ConvertTo-Json -Compress))
            # keep the Sandbox while the client stays connected (it owns the VM); a read ends on disconnect
            try { while ($pipe.IsConnected -and $null -ne $r.ReadLine()) {} } catch {}
        }
    } catch { Rec "handling FAIL $($_.Exception.Message)" }
    if ($id) { $st = (& wsb stop --id $id 2>&1 | Out-String).Trim(); Rec "client gone, wsb stop: $st" }
    try { $pipe.Dispose() } catch {}
    $pipe = New-Pipe; $wait = $pipe.WaitForConnectionAsync()
}
Rec 'END'
