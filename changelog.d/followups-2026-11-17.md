kind: portfolio-finding follow-ups: `--help`/`--version` on every binary
---
Every helper binary (`spawn-broker`, `spawn-sandbox-broker`, `spawn-jit-shim`, `spawn-probe`, `spawn-runner`, `spawn-wfp`) answers a lone `--help`/`-h`/`--version`/`-V` with exit 0 (OPS-011; `cli_basics::handle`, unit-tested with known-bad controls). The broker table's `disconnect`/`expired` no longer discard `remove`'s result: they `retain` (ERR-012). A reason on `allow(clippy::vec_box)` in `wfp.rs` (GOV-010); three stale `[reachability]` allowlist entries dropped.
