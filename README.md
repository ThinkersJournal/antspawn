# AntSpawn

Run **one job** in a **clean sandboxed environment**: create it, load only what the job declares,
run, collect the declared outputs, destroy it. Command: `spawn`. Licence: MIT OR Apache-2.0.

Launchers (`account`, `windows-sandbox`, later `sandboxie`, `hyperv`, `container`) are pluggable, and
what each can do is **tested, never asserted**: a capability is off until its row in `matrix/` says
`yes`, and every test has a known-bad control that must fail the launcher it should fail.

Status: 0.0.x. The spec types, capability set, matrix parser and launcher selection exist; the first
launchers are next. Design: [docs/DESIGN.md](docs/DESIGN.md). Launchers never elevate or create
accounts; one-time privileged setup is the operator's.

```
spawn check job.toml     # validate a job spec and list the capabilities it needs
```
