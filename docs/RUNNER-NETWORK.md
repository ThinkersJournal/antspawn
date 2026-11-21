<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Runner network policy: default deny, GitHub and crates.io only (design note)

Written 2026-11-21 at main 0f9b60c plus `work-3` for the PM's `[TASK] runner-automation-rulings`. **Design only. Nothing here was built or run; every
"measure" line is unmeasured.** Board 483 call 3 (the mechanism) is CireSnave's and is open.

CireSnave 2026-11-21, verbatim (relayed by the PM): *"The broker runs the sandbox. It can run as my account since things inside the sandbox shouldn't be able
to reach out. ... Runners likely need access to GitHub and Crates.io. Anything more should be a special exception for a specific project's runners."*

## The policy

Default deny. Allowed for every runner: GitHub (to register, fetch the job, upload logs and artifacts, clone) and crates.io (index and downloads). Anything
else is an exception **for one project's runners**, written in that project's group in the adapter config, never global.

Hosts a runner needs (**from memory, not checked against GitHub's or crates.io's docs; check before use**): `github.com`, `api.github.com`,
`codeload.github.com`, `*.actions.githubusercontent.com`, `*.githubusercontent.com`, `results-receiver.actions.githubusercontent.com`, `*.blob.core.windows.net`
(Actions artifacts and cache live on Azure Blob; a name allowlist cannot tell GitHub's accounts from anyone's), `crates.io`, `index.crates.io`,
`static.crates.io`. The Azure Blob entry is a hole: record it as a known residue, or let artifacts/cache fail.

## Where it can be enforced (the sandbox's own limits)

| Layer | What it can do | Measured here |
|---|---|---|
| `.wsb` `<Networking>` | Enable or Disable, nothing between (`runner_wsb_config` sets Enable; the job path sets Disable) | read from the code, not a limit I tested |
| Guest firewall / hosts file | Set by `job.cmd` inside the guest, **but the runner's job code runs in the guest and can undo it**: a guard against accidents, not a boundary | no |
| Host Windows Firewall | Block rules beat allow rules; rules take addresses, not names; measured 2026-11-08 that even a per-SID block-all did not see an account's loopback traffic (`src/wfp.rs` header) | partly (account only) |
| Host WFP | Connect-layer filters with a user condition work for an account (`wfp.rs`: loopback block, built, apply needs elevation). For a Sandbox guest the host sees forwarded traffic from the Default Switch (ICS, no NAT object, measured 2026-11-07): a filter would need the forward layer or the interface, which `wfp.rs` does not use | **no** |
| Hyper-V firewall | Per-VM rules need a registered VM creator; only WSL is registered (measured 2026-11-07). Whether Windows Sandbox registers one: `Get-NetFirewallHyperVVMCreator`, **not read** | no |
| A Hyper-V VM (V6) on an Internal pool switch | No route off the host by construction (forwarding Disabled, no NAT, measured 2026-11-07/08); the guest can reach only the host address on its switch, where a proxy may listen | switch part yes, proxy part no |

## Two mechanisms

**A. Address ranges.** Allow rules for GitHub's published ranges (`api.github.com/meta`) and crates.io's. Cons: crates.io's static host is behind a CDN whose
addresses move; the ranges go stale and a stale list breaks runs silently; no names (so a shared CDN address admits other sites); DNS must be allowed
(a DNS tunnel, or a second path); a block rule overrides an allow rule, so the order has to be built carefully. No new component.

**B. Filtering proxy.** The runner's network reaches exactly one address (the proxy); the proxy accepts `CONNECT host:443` only for allowlisted names, and
resolves names itself, so the guest needs no DNS (block UDP/TCP 53 outright). Per-project exceptions are one listener (port) per group, each with its own list.
Fail-closed for anything that ignores `HTTPS_PROXY` (it cannot connect at all), provided the network layer blocks direct connections. Cons: a new component
to write (a small CONNECT proxy; crates.io dependencies only), it sees host names but not content (no TLS interception; domain fronting through an allowed
CDN name is residue), and the runner, git, cargo, rustup and the Actions runner must each be pointed at it (env at start: `HTTPS_PROXY`, `NO_PROXY`; cargo
reads it; tools that do not are refused rather than leaked).

**Recommendation: B for names plus a network-layer rule that leaves the guest only the proxy address (A reduced to one address).** A alone is stale-prone;
B alone is bypassable by a direct connection. The proxy logs every refusal (host name only, no payload).

How the "only the proxy address" rule is built differs by launcher:
* **account (`ghr-*`)**: a WFP `ALE_AUTH_CONNECT` filter with the account's SID as the user condition: permit the proxy `ip:port`, block everything else. This is
  the layer `wfp.rs` already uses for loopback, so the mechanism is known to work for an account; the permit-then-block order and the remote-address
  condition are new. Elevated apply (CireSnave's). This path stays open under board 341 until this is built.
* **Windows Sandbox**: unresolved (table above). If a host-side rule cannot confine the Default Switch's guest to one address, **a network-restricted runner does not
  run in Windows Sandbox**; it runs in a Hyper-V guest (V6) on a pool switch, where "one address" is the switch's host address and the existing broker design
  already forbids a route off the host. That would make V6 and its image decision (evaluation or licensed) a dependency of default-deny on the VM path.

## Certified by tests, with controls (none run)

From inside a runner: `crates.io` and `api.github.com` succeed through the proxy; `example.com` through the proxy is refused; a **direct** connect to a public IP
bypassing the proxy fails; UDP 53 to a resolver fails; a connect to the host's loopback and own addresses fails (the Part A scan). Controls: the `naive`
network setup (Networking Enable, no filter) must FAIL the direct-connect and example.com tests, and a deliberately wrong allowlist (a project exception
missing) must refuse that project's host. A per-project exception must not appear in another project's runner.

## Measure first (all read-only or CireSnave-elevated, none done)

1. `Get-NetFirewallHyperVVMCreator` (read-only): is Windows Sandbox a registered creator? If yes, per-VM Hyper-V firewall rules may confine a Sandbox guest.
2. As `cires`, from a Sandbox guest, what path its traffic takes (`tracert`, the interface the host sees) and whether a host WFP forward-layer or interface
   filter can block it; needs an elevated apply, so CireSnave's, on a throwaway filter with an `undo`.
3. A throwaway WFP filter for `ghr-spawn1`: permit one `ip:port`, block the rest; read back. Elevated.
4. The proxy itself: not written. Size: M for the proxy, M for the filters and their read-back, S per launcher for wiring.

Nothing was run against GitHub for this note.
