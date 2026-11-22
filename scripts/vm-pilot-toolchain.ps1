# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Pilot only (docs/RUNNER-VM-PATH.md): copy a Rust + MSVC + Windows SDK + Git toolset from THIS host into the no-network pilot
  VM over PowerShell Direct and run `cargo test --locked -p aocl-scalapack` there, as the aocl selfhosted.yml does. This is a
  COPY of host files, not the Visual Studio Build Tools installer (which needs the network); it answers "does the toolchain
  run on this guest", not "does the installer work on Server Core". Run under `with-secret ANTSPAWN_PILOT_PW`.

.PARAMETER Step  Stage (tar the host files) | Copy (send and extract) | Test (build and run in the guest) | Unstage (delete the tars)
.PARAMETER Src   A clean checkout of the aocl repo with `cargo vendor --locked vendor` done and .cargo\config.toml pointing at it.
#>
param(
    [ValidateSet('Stage', 'Copy', 'Test', 'Unstage')][string]$Step = 'Stage',
    [string]$Src = '',
    [string]$Stage = 'C:\vmimages\pilot\stage',
    [string]$VmName = 'antspawn-pilot-core'
)
$ErrorActionPreference = 'Stop'
$Tar = Join-Path (Join-Path $env:windir 'System32') 'tar.exe'   # bsdtar; the PATH tar may be GNU tar, which treats C: as a host
$vs = 'C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.52.37025'
$kits = 'C:\Program Files (x86)\Windows Kits\10'
$sdkv = '10.0.28000.0'
$rustc = 'C:\rust\.rustup\toolchains\stable-x86_64-pc-windows-msvc'
function Cred { New-Object System.Management.Automation.PSCredential('Administrator', (ConvertTo-SecureString $env:ANTSPAWN_PILOT_PW -AsPlainText -Force)) }

switch ($Step) {
    'Stage' {
        New-Item -ItemType Directory -Force $Stage | Out-Null
        $t = [Diagnostics.Stopwatch]::StartNew()
        & $Tar -cf "$Stage\msvc.tar" -C $vs bin\Hostx64\x64 lib\x64 include
        & $Tar -cf "$Stage\sdk.tar" -C $kits "Lib\$sdkv\ucrt\x64" "Lib\$sdkv\um\x64" "Include\$sdkv\ucrt" "Include\$sdkv\um" "Include\$sdkv\shared"
        & $Tar -cf "$Stage\rust.tar" -C $rustc --exclude=share/doc .
        & $Tar -cf "$Stage\git.tar" -C 'C:\Program Files\Git' .
        if ($Src) { & $Tar -cf "$Stage\src.tar" -C $Src . }
        Get-ChildItem $Stage | ForEach-Object { "{0} {1:N0} MB" -f $_.Name, ($_.Length / 1MB) }
        "staged in $([math]::Round($t.Elapsed.TotalSeconds)) s"
    }
    'Copy' {
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock { New-Item -ItemType Directory -Force C:\stage, C:\msvc, C:\winsdk, C:\rust-stable, C:\Git, C:\aocl-src | Out-Null }
            $map = @{ 'msvc.tar' = 'C:\msvc'; 'sdk.tar' = 'C:\winsdk'; 'rust.tar' = 'C:\rust-stable'; 'git.tar' = 'C:\Git'; 'src.tar' = 'C:\aocl-src' }
            foreach ($k in $map.Keys) {
                $f = Join-Path $Stage $k
                if (-not (Test-Path $f)) { continue }
                $t = [Diagnostics.Stopwatch]::StartNew()
                Copy-Item -ToSession $s -Path $f -Destination "C:\stage\$k" -Force
                $copy = $t.Elapsed.TotalSeconds
                Invoke-Command -Session $s -ScriptBlock { param($k, $d) tar -xf "C:\stage\$k" -C $d; Remove-Item "C:\stage\$k" -Force } -ArgumentList $k, $map[$k]
                "{0}: copy {1:N0} s ({2:N1} MB/s), extract total {3:N0} s" -f $k, $copy, ((Get-Item $f).Length / 1MB / $copy), $t.Elapsed.TotalSeconds
            }
        } finally { Remove-PSSession $s }
    }
    'Test' {
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock {
                param($sdkv)
                $env:PATH = "C:\rust-stable\bin;C:\msvc\bin\Hostx64\x64;C:\Git\cmd;C:\AOCL-Windows\amd-blis\lib\LP64;C:\AOCL-Windows\amd-libflame\lib\LP64;C:\AOCL-Windows\amd-utils\lib;C:\AOCL-Windows\amd-scalapack\lib\LP64\shared;C:\oneapi;" + $env:PATH
                $env:LIB = "C:\msvc\lib\x64;C:\winsdk\Lib\$sdkv\ucrt\x64;C:\winsdk\Lib\$sdkv\um\x64"
                $env:INCLUDE = "C:\msvc\include;C:\winsdk\Include\$sdkv\ucrt;C:\winsdk\Include\$sdkv\um;C:\winsdk\Include\$sdkv\shared"
                $env:CARGO_HOME = 'C:\cargo-home'
                $env:CARGO_TARGET_DIR = 'C:\target'
                $env:LIBCLANG_PATH = 'C:\does-not-exist'
                $env:AOCL_ROOT = 'C:\AOCL-Windows'   # the pilot copied AOCL here, not under Program Files
                "rustc: $(rustc --version)"; "git: $(git --version)"; "link: $((Get-Command link.exe).Source)"
                Set-Location C:\aocl-src
                $t = [Diagnostics.Stopwatch]::StartNew()
                cargo test --locked --offline -p aocl-scalapack --no-fail-fast -- --nocapture 2>&1 | Select-Object -Last 40
                "cargo test exit $LASTEXITCODE after $([math]::Round($t.Elapsed.TotalSeconds)) s"
            } -ArgumentList $sdkv
        } finally { Remove-PSSession $s }
    }
    'Unstage' {
        if (Test-Path $Stage) { Remove-Item -LiteralPath $Stage -Recurse -Force }
        "stage folder exists: $(Test-Path $Stage)"
    }
}
