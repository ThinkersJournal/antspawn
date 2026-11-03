# SPDX-License-Identifier: MIT OR Apache-2.0
# GPU probe (board 371, C2 s3): can a job running as a RESTRICTED runner account see and use the GPU?
# Used two ways: by hand (`pwsh docs/proof/gpu-probe.ps1`, add -Control for the known-bad run) and, byte for byte, as the body of the
# steps in docs/proof/gpu-probe.yml (that file is generated from this one by docs/proof/make-gpu-probe-yml.py; do not edit it by hand).
# Exit 0 = the GPU is visible and answered a query. Exit 1 = it did not. With -Control the probe is run with the driver tools hidden
# (an empty PATH and no absolute path), so it MUST fail: the control passes only if it did.
param([switch]$Control)
$ErrorActionPreference = 'Stop'
function Find-NvidiaSmi {
    if ($Control) { return $null }   # known-bad: pretend the tool is not there
    $c = Get-Command nvidia-smi.exe -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    foreach ($p in 'C:\Windows\System32\nvidia-smi.exe', 'C:\Program Files\NVIDIA Corporation\NVSMI\nvidia-smi.exe') { if (Test-Path $p) { return $p } }
    $null
}
function Test-GpuVisible {
    $smi = Find-NvidiaSmi
    if (-not $smi) { throw 'nvidia-smi not found for this account' }
    $list = & $smi -L 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0 -or $list -notmatch '(?m)^GPU \d+:') { throw "nvidia-smi -L listed no GPU (exit $LASTEXITCODE): $list" }
    $q = (& $smi --query-gpu=name,driver_version,memory.total --format=csv,noheader 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $q) { throw "nvidia-smi query failed (exit $LASTEXITCODE): $q" }
    "whoami = $(whoami)"
    "gpu    = $q"
    "video controllers (informational): $((Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue | ForEach-Object Name) -join '; ')"
}
if ($Control) {
    try { Test-GpuVisible | Out-Null; throw 'CONTROL FAILED: the probe passed with the driver tools hidden' }
    catch { if ($_.Exception.Message -like 'CONTROL FAILED*') { throw }; "control held, the probe refuses without the tool: $($_.Exception.Message)" }
} else {
    Test-GpuVisible
}
