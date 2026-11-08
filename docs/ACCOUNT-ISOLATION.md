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

A Windows Firewall outbound rule for `antspawn-run`'s SID that also blocks the loopback and local interface
addresses (or a default block with named allows). It would change the `account` rows and must be re-probed:
the broker's own pipe is local and not TCP, but the V0 listener and the Windows-guest SSH path are TCP on the
host-side address, so a rule has to leave the broker's switch addresses reachable on purpose. Nothing was
written.
