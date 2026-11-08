<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# What the restricted account can reach on this machine

Scope: the `account` launcher runs a job as the local user `antspawn-run`. `matrix/launchers.tsv` says
`network.off` is `yes` for it, certified by one probe that tries to connect to an **external** address
(`probe=connect`). This page records what that row does and does not mean, measured on the machine the
runner lives on. It is a measurement and a statement of limits, not a fix: a firewall rule is an elevated
change and CireSnave's to make (PM `[TASK] measure-what-the-restricted-account-can-reach`, 2026-11-08).

## Measured 2026-11-08 (ref: main `c6e137b`; one run per cell; connect-only, no byte ever written to any socket)

Method: `spawn run <spec> --launcher account` runs a PowerShell script AS `antspawn-run` (the
`AntSpawnRunner` task, non-interactive). The script completes a TCP handshake and closes the socket at
once. The same script was run as `cires` (the interactive user) for a second column. The scripts are
`scripts/account-reach-scan.ps1` (every port on one address) and `scripts/account-reach-control.ps1` (the
table's named targets), run from a Public-folder copy with a one-job spec that mounts the script read-only.

**Control (the method can say no):** a port nothing listens on gives no answer in either column, so an
"open" is a real listener and not an artefact of the method.

| Target | `antspawn-run` | `cires` |
|---|---|---|
| `127.0.0.1` ports 1, 9, 31337, 59999 (nothing listening) | timeout (no RST), 2 s | timeout, 2 s |
| `172.25.224.1:31337`, `10.77.0.1:31337` (nothing listening) | timeout | timeout |
| `1.1.1.1:53` (external) | **refused: "forbidden by its access permissions"**, 2 ms | connected, 46 ms |
| `93.184.216.34:80` (external) | **refused: "forbidden by its access permissions"**, 14 ms | timeout (no route or no answer) |
| `127.0.0.1:135`, `:445` | **connected**, 4 ms and 1 ms | connected |
| `172.25.224.1:135`, `:445` (the Hyper-V/WSL host-side address) | **connected** | connected |
| `10.77.0.1:2222` (the V0 listener on the broker switch's host address) | **connected**, 1 ms | connected |

So the account's "network off" is real for external destinations (the OS refuses the socket outright) and
**not** for this machine's own addresses.

**Every TCP port on `127.0.0.1`, 1 to 65535, 1.5 s per batch of 1000, about 150 s:** `antspawn-run` and
`cires` each reached the **same 32 ports**; no port was open for one and not the other.

`135 445 2179 2869 4226 5037 5040 7680 7778 7899 7920 11434 13508 19294 24830 28385 28390 46367 49351 49664 49665 49666 49667 49668 49671 50496 50923 53185 53188 55382 57533 59786`

Common assignments, **not verified for this machine** (a guess from the number): 135 RPC endpoint mapper,
445 SMB, 5037 Android Debug Bridge, 5040 and 7680 Windows service ports, 11434 the default Ollama port,
49664 to 49671 dynamic RPC. The rest are unknown. `9962` (the shared Serena server named in
`C:\Projects\CLAUDE.md`) did **not** answer on `127.0.0.1`; that says only that nothing answered there at
that time.

## The limit, stated plainly

A job running as `antspawn-run` can open a TCP connection to any service listening on this machine's
loopback or host-side addresses, exactly as the interactive user can. "Network off" does not mean "nothing
on this machine is reachable". What a reached service then allows depends on that service's own
authentication, which was **not** tested: no request was sent to any of them. The risk is a local service
that trusts any local caller (a development server, a model server, a daemon with no credentials).

## The rule that exists, read back (2026-11-08, read-only: `Get-NetFirewallRule` and its security, address and port filters)

`antspawn-run: block all outbound` (created by `account-setup.ps1`): Enabled, Outbound, **Block**, Profile
Any, Protocol Any, Remote address Any, Local user = the SDDL of `antspawn-run`'s SID. Nothing else of ours
is in the firewall. So the account already has the strongest outbound rule Windows Firewall can express for
it, **and loopback and the host's own addresses still answer.** The conclusion is mechanical: a Windows
Firewall rule written by user SID does not see that traffic (the stack delivers it locally before the
firewall's rules; stated as documented Windows behaviour, shown here by an existing all-block rule that does
not bite). Only a Windows Filtering Platform filter at the connect layer, with the user as a condition, can
refuse it.

Two consequences, both from the rule above and not from a test of the changed state:

1. **"Block all outbound" and the VM broker cannot both be true.** A guest at `10.77.0.2` is a remote
   address, so while this rule stands `antspawn-run` cannot open a TCP connection to its own VM (Block beats
   Allow in Windows Firewall). V0 unmeasured item 4 ("guest reachability from a restricted account") is
   therefore answered for the current rule: **no**. The broker's own host-side address (`10.77.0.1`) is
   local and does answer.
2. **Anything that must be isolated from this machine's services belongs in a VM (or Windows Sandbox), not
   in this account.** The account isolates files, processes and the external network (the matrix rows say
   exactly that and no more); it is not a network boundary for the machine it runs on.

## Not measured

- **Which process owns each port.** Process names were asked for and cannot be obtained here:
  `Get-NetTCPConnection`, `netstat -ano` and `GetActiveTcpListeners()` return only 8 ephemeral sockets with
  process id 0 in both the tool shell and the account job, while the scan finds 32 answering ports. The tool
  shell is sandboxed, and the account job sees the same stripped table. One command by the machine's owner
  from an ordinary PowerShell window fills the column:
  `Get-NetTCPConnection -State Listen | Select-Object LocalAddress,LocalPort,@{n='Process';e={(Get-Process -Id $_.OwningProcess).ProcessName}} | Sort-Object LocalPort`
- Other addresses: the LAN-side address (`192.168.4.40`) and IPv6 (`::1`) were not scanned; only a handful of
  ports on `172.25.224.1` and `10.77.0.1` were tried (table above).
- UDP, named pipes, ALPC and COM endpoints, which a job can also reach. This page is TCP only.
- Whether a Windows Firewall rule I did not read applies per port, and what any reached service accepts.
- Whether the same holds for the other runner accounts (`ghr-spawn1`, `ghr-spawn2`); only `antspawn-run` ran.

## What a fix would need (not done; elevated; CireSnave's call)

Not a Windows Firewall rule alone (see above: the existing one does not reach local traffic). Two separate
changes, both elevated and both CireSnave's to apply, nothing written yet:

- **Windows Firewall:** replace the blanket block with "block everything except the VM pool range", so a
  runner can reach its own VM's SSH (consequence 1).
- **Windows Filtering Platform:** persistent filters at the connect layers (IPv4 and IPv6) refusing
  `antspawn-run`'s connections to loopback and to the host's own addresses. A user-id condition cannot tell a
  job's own listener from another's, so a job that starts a dev server and connects to itself would break;
  nothing in this repository's use of the account does that (our tests run as the interactive user, the
  account runs probes, `cmd` and PowerShell), a customer job might. The broker needs TCP only to the pool's
  guest addresses, which are remote.

Either must be read back after it is applied and the 32-port scan re-run as the account (expected: none open)
before the `account` rows or this page change.
