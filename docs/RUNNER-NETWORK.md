<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Runner network policy and the egress proxy (design note)

Written 2026-11-21 at main 0f9b60c plus `work-3`, for the PM's `[TASK] egress-proxy-design`. **Design only: no proxy, filter or switch rule exists; nothing here
ran except the measurements marked "measured".** The VM that carries it is `docs/RUNNER-VM-PATH.md`.

Rulings, CireSnave 2026-11-21, verbatim (relayed by the PM): *"C"* (a filtering proxy that allows named hosts; board 483 closed: broker runs the sandbox as `cires`,
key stays where it is); and *"In that case, runners that need network access run in VM, not in Sandbox. That way we can cleanly limit their network access."*
Earlier: *"Runners likely need access to GitHub and Crates.io. Anything more should be a special exception for a specific project's runners."*

So: **every runner that registers with GitHub runs in a Hyper-V VM on an internal switch with no route off the host, and reaches the world only through the
filtering proxy. Windows Sandbox stays only for jobs that need no network.** Until all of this exists, the first live job runs only on `antspawn-proof` (private,
our code) with the current network (PM).

## Why not Windows Sandbox (measured 2026-11-21, as `cires`, one guest, `Networking Enable`, no GitHub contact)

The guest is on the Default Switch: address 172.31.65.203, gateway and DNS 172.31.64.1 (the host, ICS). A host listener on 172.31.64.1:18080 (non-elevated) was reached by
the guest (CONNECTED); a direct connect to 1.1.1.1:443 also CONNECTED, so nothing confines it. `.wsb` offers only `Networking` Enable or Disable. Per-VM Hyper-V firewall rules
need a registered VM creator: only WSL is registered (read twice, 2026-11-07 and 2026-11-21). A host WFP filter on forwarded traffic might confine it; that is elevated and
was **not** run. The ruling makes it unnecessary.

## The hosts

Verified 2026-11-21 against GitHub's page "Self-hosted runners > Communication" (docs.github.com/en/actions/reference/runners/self-hosted-runners; all HTTPS 443) and
cargo's registry-index page, and by fetching `https://index.crates.io/config.json` (`dl` = `https://static.crates.io/crates`, `api` = `https://crates.io`).

| Group | Hosts | Default |
|---|---|---|
| Essential (register, get jobs) | `github.com`, `api.github.com`, `*.actions.githubusercontent.com` | allow |
| Downloading actions | `codeload.github.com` | allow |
| Logs, summaries, artifacts, caches | `results-receiver.actions.githubusercontent.com`, `*.blob.core.windows.net` | allow, see hole |
| crates.io | `index.crates.io`, `static.crates.io`, `crates.io` (git index, if used: `github.com`) | allow |
| Runner self-update | `objects.githubusercontent.com`, `objects-origin.githubusercontent.com`, `github-releases.githubusercontent.com`, `github-registry-files.githubusercontent.com` | **deny**: our runner is pinned (`runner_pin`); update the pin and base image instead. GitHub's page: updates can be turned off, but the runner must then be updated within 63 days or the service stops assigning jobs |
| Release assets | `release-assets.githubusercontent.com` | exception |
| OIDC | `*.actions.githubusercontent.com` | covered above |
| GitHub Packages / containers | `*.pkg.github.com`, `pkg-containers.githubusercontent.com`, `ghcr.io` | exception |
| Git LFS | `github-cloud.githubusercontent.com`, `github-cloud.s3.amazonaws.com` | exception |
| Dependabot | `dependabot-actions.githubapp.com` | exception |

**Known hole:** `*.blob.core.windows.net` is every Azure storage account, not only GitHub's. A name allowlist cannot narrow it. Options: accept and record; or allow it only
for a group that uploads artifacts. GitHub's page says some firewalls need rules for CNAME chains: the proxy resolves and connects by name, so it does not need that. Whether
the pinned runner needs anything beyond this list is **unmeasured** (no live run): the first run on `antspawn-proof` reads the proxy's deny log for it.

## The proxy

* **Where:** one process on the host, run by the broker's account (`cires`, ruled), written in Rust with crates.io dependencies only. It listens on the **host address of each
  pool switch** (`10.77.x.1:PORT`), never on `0.0.0.0` and never on a LAN address.
* **What it does:** accepts HTTP `CONNECT host:443` and nothing else (no plain HTTP, no other port). It decides on the target host name against the group's list, resolves
  the name itself, connects, and relays bytes. **No TLS interception** (see below).
* **One listener per group** (a port per group, or per switch). The listener a guest reached identifies its group, so per-project exceptions are a different list on a
  different listener, and a guest cannot ask for another group's list.
* **DNS only via the proxy:** the guest has no DNS server and no route; it names the proxy by IP literal; `git`, `cargo`, `rustup` and the Actions runner send host names in
  `CONNECT`. A tool that ignores `HTTPS_PROXY` simply cannot connect: fail closed.
* **How guests are forced through it:** not by trust in the guest. The switch is `Internal` with IPv4 forwarding Disabled and no NAT (measured 2026-11-07/08 on the existing
  switch `antspawn-v0-sw`, 10.77.0.1/30), so there is no path to anywhere but the host's own address. On the host, Windows Firewall inbound rules on that interface permit only the
  proxy's port and block the rest (host services, the ICS DNS, SMB, the Hyper-V WMI port): elevated setup, CireSnave's, with the host-port scan of `docs/SANDBOX-BROKER.md` as the proof.
  Unmeasured: that scan on a pool switch; that a guest cannot spoof another switch's address (one /30 per guest: the host answers only the address it assigned).

## Per-project exceptions

Declared in the adapter config, on the group: `egress = ["host:443", ...]`, extra to the default list. **Each entry needs an approval:** the config loader refuses an entry that is not in
`egress-approved.toml` (a file CireSnave's decision updates: `host`, `repo`, `approved = "board N / date"`, `reason`). So an exception is named, per project, approved, and visible in review.
Wildcards in an exception are refused unless the approval says so by name. `spawn adapter --check` prints each group's effective list. Not built.

## Logging

Every refused `CONNECT` is one line to a file the runner accounts cannot read: time, group, target host and port, reason (`not-listed`, `bad-method`, `bad-port`), guest address. Never
payload, never a header, never a URL path (the proxy cannot see it). An allowed `CONNECT` logs host and bytes at close. Size-capped with rotation. `--check` reads the last N denies so a
first live run's surprises (a host the list missed) show up as a list to approve, not as a mystery.

## Failure behaviour (all fail closed)

* Proxy not running: the guest has no way out; the runner cannot register; the job fails or stays queued with a clear message; `spawn adapter --check` probes each listener and refuses to start without it.
* Proxy crash mid-job: open tunnels drop; the job fails; the broker's lifetime cap still ends the VM.
* Unlisted host: refused, logged, the tool sees a proxy 403.
* A list that is too narrow shows as a failed job plus a deny line, never as silent success.

## TLS

**No interception.** It would need a CA the guest trusts, so a compromised proxy host could read everything and a guest could be handed forged certificates; it buys nothing here, because
the decision needs only the host name that `CONNECT` carries. Residue: `CONNECT` host and the TLS SNI can differ (domain fronting through an allowed CDN name); an optional later check peeks
the ClientHello SNI and refuses a mismatch. Not built, not measured.

## Controls the proxy and switch must pass (none run)

From a guest: `api.github.com` and `index.crates.io` succeed through the proxy; `example.com` through the proxy is refused and logged; a direct connect to a public IP, to the host's other
ports, to the ICS DNS and over UDP 53 fails; another group's exception host is refused on this group's listener. Known-bad controls: a switch with forwarding enabled (or a Default Switch guest)
must FAIL the direct-connect test; an allowlist missing a host must refuse it. Recorded in `docs/MEASUREMENTS.md` with date and ref.

## Also open: patents

ROADMAP row 20: a wider patent search must precede any publish; this design (a per-job filtering egress for ephemeral CI runners) is added to the sweep's list of ideas.
