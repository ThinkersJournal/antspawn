# SPDX-License-Identifier: MIT OR Apache-2.0
# Connect-ONLY scan of one address, ports 1-65535, as whoever runs it. A handshake is completed and the socket is
# closed at once; no byte is written. Output: one line per port that answered, plus a header and a footer.
param([string]$Target = '127.0.0.1')
$o = [System.Collections.Generic.List[string]]::new()
$o.Add('whoami ' + [Security.Principal.WindowsIdentity]::GetCurrent().Name)
$o.Add("target $Target")
$sw = [Diagnostics.Stopwatch]::StartNew()
$batch = 1000
for ($start = 1; $start -le 65535; $start += $batch) {
    $end = [Math]::Min($start + $batch - 1, 65535)
    $clients = @{}
    foreach ($p in $start..$end) {
        $c = New-Object System.Net.Sockets.TcpClient
        $clients[$p] = @{ c = $c; iar = $c.BeginConnect($Target, $p, $null, $null) }
    }
    $limit = [Diagnostics.Stopwatch]::StartNew()
    while ($limit.ElapsedMilliseconds -lt 1500 -and ($clients.Values | Where-Object { -not $_.iar.IsCompleted }).Count -gt 0) { Start-Sleep -Milliseconds 100 }
    foreach ($p in $clients.Keys) {
        $e = $clients[$p]
        if ($e.iar.IsCompleted) { try { $e.c.EndConnect($e.iar); $o.Add("open $p") } catch {} }
        try { $e.c.Close() } catch {}
    }
}
$o.Add(('done in {0} s' -f [int]$sw.Elapsed.TotalSeconds))
Set-Content -Path res.txt -Value $o
$o
