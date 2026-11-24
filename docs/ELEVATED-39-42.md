# The elevated steps for rows 39 and 42, prepared (2026-11-24, `work-5`)

Nothing here was applied. A lane runs nothing elevated; the dry runs below ran unelevated as `cires` on 2026-11-24 against the live host.
One board item for CireSnave: run the four blocks in order in ONE elevated PowerShell, from a checkout of main after this merges
(`$s = 'C:\Projects\antspawn\scripts'`). Each block has a dry run (no `-Apply`), what it changes and its undo.

## Order and why

1. **Undo the V0 leftovers** (`antspawn-v0-broker`, its two tasks, switch `antspawn-v0-sw` holding 10.77.0.1). The pool refuses to start while `10.77.0.1` answers.
2. **Pool of switches, firewall leaving only the proxy port** (row 39).
3. **Start the egress proxy** (not elevated, as `cires`), then the host-port scan from a guest (not elevated: PowerShell Direct as `cires`).
4. Row 42 needs **no elevated paste** (finding below).

The undo scripts for the runner accounts (`ghr-spawn1`, `ghr-spawn2`) are written and dry-run; they are **not** part of this item unless CireSnave wants those accounts gone before the first VM job: `antspawn-account` still uses them.

## 1. Undo the V0 broker

| | |
|---|---|
| Command | `& "$s\broker-v0.ps1" -Undo` |
| Dry run | `scripts\broker-v0.ps1` with no flag prints the *apply* plan only; **`-Undo` has no dry run** (it needs elevation to read). Read-only state today: user `antspawn-v0-broker` Enabled, member of `Hyper-V Administrators`, tasks `AntSpawnV0Probe` and `AntSpawnV0Listen` Ready, switch `antspawn-v0-sw` with `10.77.0.1` (all read 2026-11-24). |
| Removes | both tasks, any `antspawn-v0-*` VM/switch/disks, the 10.77.0.1 address, the group membership and batch right, the user, `%ProgramData%\antspawn-v0`; reads back PASS/FAIL |
| Leaves | every other account, switch, VM and rule |

## 2. The pool, with the proxy port left open

| | |
|---|---|
| Dry run (ran) | `& "$s\broker-pool.ps1" -ProxyPort 3128` printed 4 PLAN lines (switches `antspawn-sw1..4`, `10.77.i.1/30`, forwarding Disabled, no NAT) and `REFUSE something already answers to a 10.77.x address (10.77.0.1 on vEthernet (antspawn-v0-sw))`: step 1 clears that |
| Command | `& "$s\broker-pool.ps1" -ProxyPort 3128 -Apply` |
| Changes | 4 Internal switches, a /30 host address each, `Set-NetIPInterface -Forwarding Disabled`, and per switch three inbound **Block** rules bound to its interface: TCP on every port except 3128 (`1-3127,3129-65535`), UDP, ICMPv4. (Windows Firewall cannot say "block all but one port" and a Block beats an Allow, hence ranges.) State in `%ProgramData%\antspawn-pool\state.json` (Administrators + SYSTEM). Reads each slot back and stops at the first mismatch |
| Undo | `& "$s\broker-pool.ps1" -Undo` removes only what `state.json` names, reads back |
| Not exercised by the dry run | `New-VMSwitch`, the address, `New-NetFirewallRule -InterfaceAlias` on a fresh interface, the port-filter read-back. **`-ProxyPort` is new in this branch: UNRUN beyond parsing** |
| Design point for the PM | the proxy's listener for a group is `10.77.i.1:3128` of ONE slot, but a guest reaches only its own slot's host address. So a group's `egress_listen` is per slot, or one listener per slot sharing the group's list. Decide before the first guest run (the config has one `egress_listen` per group today) |

## 3. Proxy and scan (no elevation)

`spawn-egress --config <adapter.toml> --log-dir <dir>` as `cires`, then `spawn adapter --check` (probes each listener), then from a guest on a pool switch: TCP/UDP to every host port except 3128 must fail, DNS must fail, another switch's host address must fail. Known-bad control: the same scan with `-ProxyPort 0` removed from the rules must show 3128 closed too (rules working) and, with the rules removed in a throwaway slot, the ports open (the scan can see them).

## 4. Row 42 (PowerShell Direct as the broker account)

CireSnave ruled the broker runs as `cires` (board 483, "C"), and `cires` is in `Hyper-V Administrators`. The V7 paste (`scripts/broker-account.ps1`, account `antspawn-broker`) was for a *dedicated* account that ruling superseded. PowerShell Direct, `New-VM` from a differencing child, boot-to-answer time (about 12 s) and guest start/stop are already run as `cires`, non-elevated, by the pilot (`docs/RUNNER-VM-PATH.md`, `src/hyperv.rs`). What row 42 still lacks is only **guest-to-host and guest-to-guest scans**, which need step 2. So: no elevated item for row 42; PM to confirm with CireSnave that V7's dedicated account is dropped.

## Runner-account undo (written, dry-run, not part of the item)

`scripts\runner-account-undo.ps1 -Name spawn1` (and `spawn2`): dry run read the task (Ready), the account, the root `C:\gh-runner-spawn1`, and printed the plan; the batch-right read needs elevation and says UNKNOWN unelevated. `-Apply` unelevated is refused. `-Name 'Bad Name'` and a name with no account are handled (positive control: `spawn1` present, `nosuch` absent). `-Apply` removes the task, only that SID's batch right, the user and the root, and reads each back. UNRUN as `-Apply`.
