# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Pilot (docs/RUNNER-VM-PATH.md): install the toolchain INSIDE the no-network Server Core pilot VM from installers kept on this
  host (C:\vmimages\installers: VS 2022 Build Tools layout, vc_redist, Git for Windows, the Rust MSI), unattended, then run the
  aocl-scalapack cargo test from the installed toolchain and (Bake) fold the result into a second base VHDX.
  Run as the interactive Hyper-V Administrator under `with-secret ANTSPAWN_PILOT_PW`; never elevates; passes no network.
  The Build Tools accept flag is used only because CireSnave accepted that licence for the runner image (board 486, 2026-11-21).
.PARAMETER Step  Copy | BuildTools | Install | Test | Runner | Vendor | Mpi | Bake   (BuildTools first: it imports the layout's Microsoft root certificates the offline installer needs, then installs; Install does vc_redist, Git, Rust)
#>
param(
    [ValidateSet('Copy', 'BuildTools', 'Install', 'Test', 'Runner', 'Vendor', 'Mpi', 'Src', 'Bake')][string]$Step = 'Copy',
    [string]$VmName = 'antspawn-pilot-core',
    [string]$Inst = 'C:\vmimages\installers',
    [string]$Stage = 'C:\vmimages\pilot\stage',
    [string]$Src = '',
    [string]$Work = 'C:\vmimages\pilot',
    [string]$Out = 'core-tools-base.vhdx',
    [switch]$KeepVendor   # Bake: keep C:\AOCL-Windows, C:\IntelMPI, C:\oneapi and C:\docs (board 487 ruling A, 2026-11-21)
)
$ErrorActionPreference = 'Stop'
$Tar = Join-Path (Join-Path $env:windir 'System32') 'tar.exe'
function Cred { New-Object System.Management.Automation.PSCredential('Administrator', (ConvertTo-SecureString $env:ANTSPAWN_PILOT_PW -AsPlainText -Force)) }

switch ($Step) {
    'Copy' {
        New-Item -ItemType Directory -Force $Stage | Out-Null
        & $Tar -cf "$Stage\inst.tar" -C $Inst bt-layout vs_BuildTools.exe vc_redist.x64.exe Git-2.56.1-64-bit.exe rust-1.100.0-x86_64-pc-windows-msvc.msi
        if ($Src) { & $Tar -cf "$Stage\src.tar" -C $Src . }
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock { New-Item -ItemType Directory -Force C:\stage, C:\inst, C:\aocl-src | Out-Null }
            foreach ($p in @(@('inst.tar', 'C:\inst'), @('src.tar', 'C:\aocl-src'))) {
                $f = Join-Path $Stage $p[0]; if (-not (Test-Path $f)) { continue }
                $t = [Diagnostics.Stopwatch]::StartNew()
                Copy-Item -ToSession $s -Path $f -Destination "C:\stage\$($p[0])" -Force
                Invoke-Command -Session $s -ScriptBlock { param($k, $d) & (Join-Path $env:windir 'System32\tar.exe') -xf "C:\stage\$k" -C $d; Remove-Item "C:\stage\$k" -Force } -ArgumentList $p[0], $p[1]
                "{0}: {1:N0} MB, {2:N0} s" -f $p[0], ((Get-Item $f).Length / 1MB), $t.Elapsed.TotalSeconds
            }
        } finally { Remove-PSSession $s }
        Remove-Item -LiteralPath $Stage -Recurse -Force; "stage folder exists: $(Test-Path $Stage)"
    }
    'BuildTools' {
        # Offline, the installer cannot fetch the intermediate "Microsoft Windows Code Signing PCA 2024" that signs vs_installer.opc
        # (measured 2026-11-21: PartialChain, then bootstrapper error 0x80131509). Build the chain on THIS host (online) and send
        # the intermediate (not the root) to the guest's CA store. --lang is not accepted by `setup.exe install`; Response.json has the language.
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $z = [IO.Compression.ZipFile]::OpenRead((Join-Path $Inst 'bt-layout\vs_installer.opc'))
        try { $e = $z.Entries | Where-Object FullName -match 'digital-signature/xml-signature/.*\.psdsxs$' | Select-Object -First 1; $sr = New-Object IO.StreamReader($e.Open()); $xml = $sr.ReadToEnd(); $sr.Close() } finally { $z.Dispose() }
        $b64 = @([regex]::Matches($xml, '<X509Certificate>([^<]+)</X509Certificate>') | ForEach-Object { $_.Groups[1].Value })
        $leaf = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2(,[Convert]::FromBase64String($b64[0]))
        $chain = New-Object System.Security.Cryptography.X509Certificates.X509Chain
        if (-not $chain.Build($leaf)) { throw 'cannot build the signing chain on the host' }
        $inter = $chain.ChainElements[1].Certificate
        New-Item -ItemType Directory -Force (Join-Path $Inst 'bt-chain') | Out-Null
        [IO.File]::WriteAllBytes((Join-Path $Inst 'bt-chain\chain1.cer'), $inter.RawData)
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Copy-Item -ToSession $s -Path (Join-Path $Inst 'bt-chain\chain1.cer') -Destination 'C:\inst\chain1.cer' -Force
            Invoke-Command -Session $s -ScriptBlock { certutil.exe -addstore -f CA C:\inst\chain1.cer | Out-Null; "imported intermediate, exit $LASTEXITCODE" }
            foreach ($f in Get-ChildItem (Join-Path $Inst 'bt-layout\certificates') -Filter *.cer) {
                Copy-Item -ToSession $s -Path $f.FullName -Destination "C:\inst\bt-layout\certificates\$($f.Name)" -Force -ErrorAction SilentlyContinue
            }
            Invoke-Command -Session $s -ScriptBlock {
                # offline installs cannot validate the chain: Microsoft documents importing the layout's certificates (guest-local trust)
                foreach ($c in Get-ChildItem 'C:\inst\bt-layout\certificates' -Filter *.cer) {
                    $x = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($c.FullName)
                    certutil.exe -addstore -f Root $c.FullName | Out-Null
                    "imported $($c.Name): subject $($x.Subject), thumbprint $($x.Thumbprint), exit $LASTEXITCODE"
                }
                $t = [Diagnostics.Stopwatch]::StartNew()
                $p = Start-Process -FilePath 'C:\inst\bt-layout\vs_BuildTools.exe' -ArgumentList @('--noweb', '--quiet', '--wait', '--norestart', '--nocache', '--installPath', 'C:\BuildTools', '--add', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '--add', 'Microsoft.VisualStudio.Component.Windows11SDK.26100') -Wait -PassThru
                "Build Tools (layout, offline): exit $($p.ExitCode) in $([math]::Round($t.Elapsed.TotalSeconds)) s"
                'CRT redist from Build Tools: ' + ((Get-ChildItem C:\BuildTools\VC\Redist -Recurse -Filter vcruntime140.dll -ErrorAction SilentlyContinue | ForEach-Object FullName) -join '; ')
                $n = 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll', 'vcomp140.dll'
                'VC++ in System32 (after Build Tools): ' + (($n | ForEach-Object { "$_=$(Test-Path (Join-Path $env:windir "System32\$_"))" }) -join ' ')
                $v = Get-Volume -DriveLetter C; 'C: used {0:N2} GB' -f (($v.Size - $v.SizeRemaining) / 1GB)
            }
        } finally { Remove-PSSession $s }
    }
    'Install' {
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock {
                function Vc([string]$when) {
                    $n = 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll', 'vcomp140.dll'
                    "VC++ in System32 ($when): " + (($n | ForEach-Object { "$_=$(Test-Path (Join-Path $env:windir "System32\$_"))" }) -join ' ')
                }
                function Run([string]$what, [string]$exe, [string[]]$arg) {
                    $t = [Diagnostics.Stopwatch]::StartNew()
                    $p = Start-Process -FilePath $exe -ArgumentList $arg -Wait -PassThru
                    "{0}: exit {1} in {2:N0} s" -f $what, $p.ExitCode, $t.Elapsed.TotalSeconds
                }
                Vc 'stock Core'
                Vc 'after Build Tools'
                'CRT redist folder from Build Tools: ' + ((Get-ChildItem C:\BuildTools\VC\Redist -Recurse -Filter vcruntime140.dll -ErrorAction SilentlyContinue | ForEach-Object FullName) -join '; ')
                Run 'vc_redist.x64' 'C:\inst\vc_redist.x64.exe' @('/install', '/quiet', '/norestart')
                Vc 'after vc_redist'
                Run 'Git for Windows' 'C:\inst\Git-2.56.1-64-bit.exe' @('/VERYSILENT', '/NORESTART', '/SUPPRESSMSGBOXES', '/NOCANCEL', '/SP-')
                Run 'Rust MSI' 'msiexec.exe' @('/i', 'C:\inst\rust-1.100.0-x86_64-pc-windows-msvc.msi', '/qn', '/norestart')
                $v = Get-Volume -DriveLetter C
                'C: used {0:N2} GB of {1:N1} GB' -f (($v.Size - $v.SizeRemaining) / 1GB), ($v.Size / 1GB)
                'rust dir: ' + ((Get-ChildItem 'C:\Program Files' -Directory -Filter 'Rust*' | ForEach-Object FullName) -join '; ')
            }
        } finally { Remove-PSSession $s }
    }
    'Test' {
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock {
                $rust = (Get-ChildItem 'C:\Program Files' -Directory -Filter 'Rust*' | Select-Object -First 1).FullName
                # no LIB/INCLUDE and no vcvars: cargo/rustc find MSVC through the Build Tools' own registration
                $env:PATH = "$rust\bin;C:\Program Files\Git\cmd;C:\IntelMPI\bin;C:\AOCL-Windows\amd-blis\lib\LP64;C:\AOCL-Windows\amd-libflame\lib\LP64;C:\AOCL-Windows\amd-utils\lib;C:\AOCL-Windows\amd-scalapack\lib\LP64\shared;C:\oneapi;" + $env:PATH
                $env:AOCL_ROOT = 'C:\AOCL-Windows'; $env:LIBCLANG_PATH = 'C:\does-not-exist'
                $env:CARGO_HOME = 'C:\cargo-home'; $env:CARGO_TARGET_DIR = 'C:\target'
                "rustc: $(rustc --version)"; "cargo: $(cargo --version)"; "git: $(git --version)"
                Set-Location C:\aocl-src
                $t = [Diagnostics.Stopwatch]::StartNew()
                cargo test --locked --offline -p aocl-scalapack --no-fail-fast -- --nocapture 2>&1 | Select-Object -Last 25
                "cargo test exit $LASTEXITCODE after $([math]::Round($t.Elapsed.TotalSeconds)) s"
            }
        } finally { Remove-PSSession $s }
    }
    'Runner' {
        # the pinned Actions runner (src/adapter/runner_pin.rs: 2.338.1): verify the zip against the pin BEFORE it goes in; no config, no credentials
        $zip = Join-Path $Inst 'actions-runner-win-x64-2.338.1.zip'
        $h = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLower()
        if ($h -ne '478f533ac2c349040fbaea7e158f7f7d0235085cf35bc81f661af0c715207ff9' -or (Get-Item $zip).Length -ne 104210469) { throw "runner zip does not match the pin: $h" }
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock { New-Item -ItemType Directory -Force C:\stage, C:\runner | Out-Null }
            Copy-Item -ToSession $s -Path $zip -Destination 'C:\stage\runner.zip' -Force
            Invoke-Command -Session $s -ScriptBlock {
                & (Join-Path $env:windir 'System32\tar.exe') -xf C:\stage\runner.zip -C C:\runner
                Remove-Item C:\stage -Recurse -Force
                'runner dir: ' + ((Get-ChildItem C:\runner -Force | ForEach-Object Name) -join ', ')
                "Runner.Listener --version: $(& C:\runner\bin\Runner.Listener.exe --version 2>&1) (exit $LASTEXITCODE)"
                $v = Get-Volume -DriveLetter C; 'C: used {0:N2} GB' -f (($v.Size - $v.SizeRemaining) / 1GB)
            }
        } finally { Remove-PSSession $s }
    }
    'Vendor' {
        # board 487 ruling A (CireSnave 2026-11-21): the host's AOCL tree, Intel MPI and the five Intel runtime DLLs go into the image
        # (laptop-local, read-only, never copied elsewhere) with their licence texts in C:\docs\licences and a sha256 manifest.
        $aocl = Join-Path $env:ProgramFiles 'AMD\AOCL-Windows'
        $mpi = (Get-Item -LiteralPath (Join-Path ${env:ProgramFiles(x86)} 'Intel\oneAPI\mpi\latest')).Target
        if (-not $mpi) { $mpi = Join-Path ${env:ProgramFiles(x86)} 'Intel\oneAPI\mpi\2021.18' }
        $mpi = "$mpi".TrimStart('\?')   # a link target can carry a \\?\ prefix
        $oneapi = Join-Path ${env:ProgramFiles(x86)} 'Intel\oneAPI'
        $dll = foreach ($f in 'impi.dll', 'libifcoremd.dll', 'libmmd.dll', 'svml_dispmd.dll', 'libiomp5md.dll') {
            # the umbrella folder holds 0-byte links: take the first real, 64-bit file (the lookup rule vm-pilot.ps1 Oneapi uses)
            $h = Get-ChildItem -LiteralPath $oneapi -Filter $f -Recurse -ErrorAction SilentlyContinue | Where-Object { -not $_.LinkType -and $_.Length -gt 0 -and $_.FullName -notmatch 'bin32|ia32|[\\/]x86[\\/]|win32' } | Select-Object -First 1
            if (-not $h) { throw "$f not found as a real file under $oneapi" }
            $h
        }
        New-Item -ItemType Directory -Force $Stage | Out-Null
        $man = New-Object System.Collections.Generic.List[string]
        $man.Add("# sha256, size, guest path, host source path. Generated $(Get-Date -Format 'yyyy-MM-dd') on the host that built this image.")
        $add = {
            param($root, $guestRoot)
            foreach ($f in Get-ChildItem -LiteralPath $root -Recurse -File -Force) {
                $rel = $f.FullName.Substring($root.TrimEnd('\').Length + 1)
                $man.Add(('{0}  {1}  {2}  {3}' -f (Get-FileHash -Algorithm SHA256 -LiteralPath $f.FullName).Hash.ToLower(), $f.Length, (Join-Path $guestRoot $rel), $f.FullName))
            }
        }
        & $add $aocl 'C:\AOCL-Windows'
        & $add $mpi 'C:\IntelMPI'
        foreach ($d in $dll) { $man.Add(('{0}  {1}  {2}  {3}' -f (Get-FileHash -Algorithm SHA256 -LiteralPath $d.FullName).Hash.ToLower(), $d.Length, "C:\oneapi\$($d.Name)", $d.FullName)) }
        $lic = Join-Path $Inst 'vendor-licences'
        foreach ($f in Get-ChildItem -LiteralPath $lic -File) { $man.Add(('{0}  {1}  {2}  {3}' -f (Get-FileHash -Algorithm SHA256 -LiteralPath $f.FullName).Hash.ToLower(), $f.Length, "C:\docs\licences\$($f.Name)", $f.FullName)) }
        $manf = Join-Path $Inst 'vendor-MANIFEST-sha256.txt'
        [IO.File]::WriteAllLines($manf, $man)
        "manifest: $($man.Count - 1) entries -> $manf"
        & $Tar -cf "$Stage\aocl.tar" -C $aocl .
        & $Tar -cf "$Stage\mpi.tar" -C $mpi .
        New-Item -ItemType Directory -Force "$Stage\dll" | Out-Null; foreach ($d in $dll) { Copy-Item -LiteralPath $d.FullName "$Stage\dll\$($d.Name)" }
        & $Tar -cf "$Stage\dll.tar" -C "$Stage\dll" .
        & $Tar -cf "$Stage\lic.tar" -C $lic .
        if ($Src) { & $Tar -cf "$Stage\src.tar" -C $Src . }   # the aocl checkout the Test step builds (removed again before Bake)
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock { New-Item -ItemType Directory -Force C:\stage, C:\AOCL-Windows, C:\IntelMPI, C:\oneapi, C:\docs\licences, C:\aocl-src | Out-Null }
            $items = @(@('aocl.tar', 'C:\AOCL-Windows'), @('mpi.tar', 'C:\IntelMPI'), @('dll.tar', 'C:\oneapi'), @('lic.tar', 'C:\docs\licences'))
            if ($Src) { $items += ,@('src.tar', 'C:\aocl-src') }
            foreach ($p in $items) {
                $f = Join-Path $Stage $p[0]; $t = [Diagnostics.Stopwatch]::StartNew()
                Copy-Item -ToSession $s -Path $f -Destination "C:\stage\$($p[0])" -Force
                Invoke-Command -Session $s -ScriptBlock { param($k, $d) & (Join-Path $env:windir 'System32\tar.exe') -xf "C:\stage\$k" -C $d; Remove-Item "C:\stage\$k" -Force } -ArgumentList $p[0], $p[1]
                "{0}: {1:N0} MB, {2:N0} s" -f $p[0], ((Get-Item $f).Length / 1MB), $t.Elapsed.TotalSeconds
            }
            Copy-Item -ToSession $s -Path $manf -Destination 'C:\docs\vendor-MANIFEST-sha256.txt' -Force
            # read back in the guest: every baked file hashes to its manifest line
            Invoke-Command -Session $s -ScriptBlock {
                $bad = 0; $n = 0
                foreach ($l in Get-Content C:\docs\vendor-MANIFEST-sha256.txt | Where-Object { $_ -notmatch '^#' }) {
                    $q = $l -split '  ', 4; $n++
                    if (-not (Test-Path -LiteralPath $q[2]) -or (Get-FileHash -Algorithm SHA256 -LiteralPath $q[2]).Hash.ToLower() -ne $q[0]) { $bad++; "MISMATCH $($q[2])" }
                }
                "guest read-back: $n files, $bad mismatches"
                $v = Get-Volume -DriveLetter C; 'C: used {0:N2} GB' -f (($v.Size - $v.SizeRemaining) / 1GB)
            }
        } finally { Remove-PSSession $s }
        Remove-Item -LiteralPath $Stage -Recurse -Force; "stage folder exists: $(Test-Path $Stage)"
    }
    'Mpi' {
        # a real single-rank launch with the baked Intel MPI, offline (no network in the guest)
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock {
                $env:PATH = 'C:\IntelMPI\bin;C:\oneapi;' + $env:PATH
                'mpiexec: ' + (Get-Command mpiexec.exe).Source
                $t = [Diagnostics.Stopwatch]::StartNew()
                $o = & mpiexec.exe -n 1 cmd.exe /c echo rank-launched 2>&1; "mpiexec -n 1 cmd: exit $LASTEXITCODE after $([math]::Round($t.Elapsed.TotalSeconds,1)) s :: $($o -join ' | ')"
                $t = [Diagnostics.Stopwatch]::StartNew()
                $o = & mpiexec.exe -localonly -n 1 cmd.exe /c echo rank-launched-localonly 2>&1; "mpiexec -localonly -n 1 cmd: exit $LASTEXITCODE after $([math]::Round($t.Elapsed.TotalSeconds,1)) s :: $($o -join ' | ')"
                $o = & C:\IntelMPI\bin\impi_info.exe -a 2>&1 | Select-Object -First 3; "impi_info: exit $LASTEXITCODE :: $($o -join ' | ')"
            }
        } finally { Remove-PSSession $s }
    }
    'Src' {
        # only the source checkout the Test step builds (not a mount: a tar sent over PowerShell Direct)
        New-Item -ItemType Directory -Force $Stage | Out-Null
        & $Tar -cf "$Stage\src.tar" -C $Src .
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock { New-Item -ItemType Directory -Force C:\stage, C:\aocl-src | Out-Null }
            Copy-Item -ToSession $s -Path "$Stage\src.tar" -Destination 'C:\stage\src.tar' -Force
            Invoke-Command -Session $s -ScriptBlock { & (Join-Path $env:windir 'System32\tar.exe') -xf C:\stage\src.tar -C C:\aocl-src; Remove-Item C:\stage -Recurse -Force }
        } finally { Remove-PSSession $s }
        Remove-Item -LiteralPath $Stage -Recurse -Force
    }
    'Bake' {
        $child = Join-Path $Work 'child.vhdx'; $out = Join-Path $Work $Out
        if (Test-Path -LiteralPath $out) { throw "$out exists: refuse to overwrite" }
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            # tidy what must not be baked: build output, cargo home, stage, installers, the vendor AOCL/Intel copies (their licences are unread), the guest-only WinTrust tweak
            Invoke-Command -Session $s -ArgumentList ([bool]$KeepVendor) -ScriptBlock { param($k) $v = Get-Volume -DriveLetter C; 'C: used before tidy {0:N2} GB' -f (($v.Size - $v.SizeRemaining) / 1GB); $rm = @('C:\target', 'C:\cargo-home', 'C:\stage', 'C:\inst', 'C:\aocl-src'); if (-not $k) { $rm += 'C:\AOCL-Windows', 'C:\oneapi', 'C:\IntelMPI', 'C:\docs' }; Remove-Item $rm -Recurse -Force -ErrorAction SilentlyContinue; Get-ChildItem Cert:\LocalMachine\CA | Where-Object Thumbprint -eq 'D30F05F637E605239C0070D1EA9860D434AC2A94' | Remove-Item; Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\WinTrust\Trust Providers\Software Publishing' -Name State -ErrorAction SilentlyContinue; $v = Get-Volume -DriveLetter C; 'C: used after tidy {0:N2} GB' -f (($v.Size - $v.SizeRemaining) / 1GB); Optimize-Volume -DriveLetter C -ReTrim | Out-Null }
        } finally { Remove-PSSession $s }
        Stop-VM -Name $VmName -Force
        Remove-VM -Name $VmName -Force
        Convert-VHD -Path $child -DestinationPath $out -VHDType Dynamic
        Set-ItemProperty -LiteralPath $out -Name IsReadOnly -Value $true
        Remove-Item -LiteralPath $child -Force
        "baked $out ({0:N2} GB on disk), read-only {1}; child removed: {2}" -f ((Get-Item $out).Length / 1GB), (Get-Item $out).IsReadOnly, (-not (Test-Path $child))
    }
}
