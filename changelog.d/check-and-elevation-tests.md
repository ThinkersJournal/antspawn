kind: **ADDED**: tests for `spawn check` rejections and for the launchers never elevating
---
README said neither claim had a test. `tests/cli.rs` now runs `spawn check` on a valid spec (accepted, needs listed) and on eight bad ones (not TOML, empty cmd, empty name, secrets, scratch mount with host, ro/rw mount without host, bad output name, reserved output name), each non-zero with its reason on stderr. `tests/never_elevates.rs` scans `git ls-files` sources for elevation and account-creation primitives, with a planted-line control; it is a source scan, not a runtime proof. README sentence removed. Verified: `cargo test --test cli --test never_elevates`.

— lane: antspawn
