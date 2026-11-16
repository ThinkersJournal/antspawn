kind: CodeRipper pin moves to 0.47.5
---
The shared portfolio workflow call in `.github/workflows/ci.yml` moves from `74996d7` to `c72a6e03` (CodeRipper 0.47.5). `migrate-rule-ids.py --dry-run` at that SHA exits 0: this repo's `.coderipper.toml` names only `SEC-002` and `reachability`, so no rule id needed renaming. No code change.
