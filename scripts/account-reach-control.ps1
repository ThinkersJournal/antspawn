# SPDX-License-Identifier: MIT OR Apache-2.0
$o = [System.Collections.Generic.List[string]]::new()
$o.Add('whoami ' + [Security.Principal.WindowsIdentity]::GetCurrent().Name)
foreach ($t in @(@('127.0.0.1',1),@('127.0.0.1',9),@('127.0.0.1',31337),@('127.0.0.1',59999),@('10.77.0.1',31337),@('172.25.224.1',31337),@('127.0.0.1',135),@('127.0.0.1',445),@('127.0.0.1',2222),@('10.77.0.1',2222),@('10.77.0.1',22),@('93.184.216.34',80),@('1.1.1.1',53))) {
    $sw = [Diagnostics.Stopwatch]::StartNew(); $res = 'error'
    try {
        $c = New-Object System.Net.Sockets.TcpClient
        $iar = $c.BeginConnect($t[0], $t[1], $null, $null)
        if ($iar.AsyncWaitHandle.WaitOne(2000)) { try { $c.EndConnect($iar); $res = 'CONNECT' } catch { $res = 'refused: ' + $_.Exception.InnerException.Message } } else { $res = 'timeout 2000 ms' }
        $c.Close()
    } catch { $res = 'error ' + $_.Exception.Message }
    $o.Add(('{0}:{1}  {2}  ({3} ms)' -f $t[0], $t[1], $res, $sw.ElapsedMilliseconds))
}
Set-Content -Path res.txt -Value $o
$o
