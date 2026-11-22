kind: **ADDED**: `spawn adapter` checks each group's egress list against `egress-approved.toml`
---
`spawn adapter --check` now reads `egress-approved.toml` beside the config file and prints each group's effective egress list; `spawn adapter` refuses to start when a group's `egress` entry is not approved for every repo of that group (`command::load_egress`). A missing file counts as empty, so a config with no extras needs none. 1 test with a known-bad control (the same entry refused with no file, accepted once approved, a malformed file named in the error). Listener probes wait for the pool switches (ROADMAP row 39).

Also `adapter::vm_backend` (ROADMAP row 40, not wired to any group): `VmBackend` runs one runner per Hyper-V VM (a differencing child of the read-only base, one NIC on a pool switch, no checkpoints), verifies the base hash once and destroys the VM on every exit path, with a `VmHost` seam and `ScriptHost` building the PowerShell as quoted, prefix-checked text; 12 tests. The guest channel (JIT delivery, runner-end wait) is not built.

And `spawn-egress --check-sni`: the proxy reads the first TLS record and refuses a tunnel whose SNI is not the `CONNECT` host (domain fronting through an allowed name); a parser that never indexes past its buffer, 3 tests, measured live on loopback.
