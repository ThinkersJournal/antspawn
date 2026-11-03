# SPDX-License-Identifier: MIT OR Apache-2.0
# ONE command for the account-backed proof once C1 (the GitHub App is created and installed on ThinkersJournal/antspawn-proof) is done.
# NOT RUN YET: written from the adapter's documented flags (`--check`, `--once`; exit 3 = a runner ended in an error). Run as cires, not elevated.
# Prerequisites it checks and refuses on: runner group antspawn-account-2 exists, proof.yml and gpu-probe.yml are on antspawn-proof main.
# Without -Go it only prints the plan and runs the read-only checks (--check, the GitHub probe).
param([switch]$Go, [string]$Spawn = 'C:\Projects\antspawn\target\release\spawn.exe', [string]$Config = 'C:\Projects\antspawn\docs\proof\adapter.toml',
      [string]$Repo = 'ThinkersJournal/antspawn-proof', [int]$Runs = 3)
$ErrorActionPreference = 'Stop'
$log = "C:\Users\Public\antspawn-proof-$(Get-Date -Format yyyyMMdd-HHmmss).log"
Start-Transcript -Path $log | Out-Null
$rows = [System.Collections.Generic.List[object]]::new()
function Row([string]$What, [bool]$Ok, [string]$Detail = '') { $rows.Add([pscustomobject]@{ Step = $What; Result = $(if ($Ok) { 'PASS' } else { 'FAIL' }); Detail = $Detail }); Write-Host ("{0,-5} {1} {2}" -f $(if ($Ok) { 'PASS' } else { 'FAIL' }), $What, $Detail) }
try {
    if (-not (Test-Path $Spawn)) { throw "no ${Spawn}: build it first (cargo build --release --bin spawn)" }
    & $Spawn adapter --config $Config --check; Row 'spawn adapter --check' ($LASTEXITCODE -eq 0) "exit $LASTEXITCODE"
    Push-Location C:\Projects\antspawn; try { cargo run --release --example github_proof -- probe $Config; Row 'GitHub probe (read-only)' ($LASTEXITCODE -eq 0) "exit $LASTEXITCODE" } finally { Pop-Location }
    $groups = gh api orgs/ThinkersJournal/actions/runner-groups --jq '.runner_groups[].name'
    Row 'runner group antspawn-account-2 exists' ($groups -contains 'antspawn-account-2') ($groups -join ',')
    $wf = gh api repos/$Repo/actions/workflows --jq '.workflows[].path'
    Row 'workflows pushed (proof.yml, gpu-probe.yml)' (($wf -contains '.github/workflows/proof.yml') -and ($wf -contains '.github/workflows/gpu-probe.yml')) ($wf -join ',')
    if (-not $Go) { Write-Host "`nPlan only (no -Go): would dispatch proof.yml $Runs times, run '$Spawn adapter --config <cfg> --once' after each, read each run's conclusion, then dispatch gpu-probe.yml once."; return }
    if ($rows | Where-Object Result -eq 'FAIL') { throw 'a precondition failed; nothing dispatched' }
    foreach ($n in 1..$Runs) {
        gh workflow run proof.yml --repo $Repo; Start-Sleep 5
        & $Spawn adapter --config $Config --once; $once = $LASTEXITCODE
        Start-Sleep 5
        $c = gh run list --repo $Repo --workflow proof.yml --limit 1 --json conclusion,databaseId --jq '.[0] | "\(.databaseId) \(.conclusion)"'
        Row "proof run $n" (($once -eq 0) -and ($c -match ' success$')) "--once exit $once; run $c"
    }
    gh workflow run gpu-probe.yml --repo $Repo; Start-Sleep 5
    & $Spawn adapter --config $Config --once; $once = $LASTEXITCODE; Start-Sleep 5
    $c = gh run list --repo $Repo --workflow gpu-probe.yml --limit 1 --json conclusion,databaseId --jq '.[0] | "\(.databaseId) \(.conclusion)"'
    Row 'gpu-probe run' (($once -eq 0) -and ($c -match ' success$')) "--once exit $once; run $c"
} catch { Row 'aborted' $false $_.Exception.Message }
finally {
    Write-Host "`n===== SUMMARY"; $rows | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
    Stop-Transcript | Out-Null; "transcript: $log (holds no secret: the adapter never prints the config)"
}
