# AntSpawn

Run **one job** in a **clean sandboxed environment**: create it, load only what the job declares,
run, collect the declared outputs, destroy it. Command: `spawn`. Licence: MIT OR Apache-2.0.

Launchers (`account`, `windows-sandbox`, later `sandboxie`, `hyperv`, `container`) are pluggable, and
what each can do is **tested, never asserted**: a capability is off until its row in `matrix/` says
`yes`, and every test has a known-bad control that must fail the launcher it should fail.

Status: 0.0.x. Two launchers are certified by `matrix/launchers.tsv`: `account` (a restricted Windows account
on the host) and `windows-sandbox` (a fresh Windows Sandbox VM); `naive` is the known-bad control that fails every
test. Which capability each has is whatever `spawn matrix` prints; nothing here overrides it. Design:
[docs/DESIGN.md](docs/DESIGN.md); measurements, with their dates and refs: [docs/MEASUREMENTS.md](docs/MEASUREMENTS.md);
work queue: [ROADMAP.md](ROADMAP.md). Launchers never elevate or create accounts; one-time privileged setup is the
operator's, and a launcher that finds it missing says so and stops.

```
spawn check job.toml          # validate a job spec and list the capabilities it needs
spawn providers job.toml      # every launcher, those that satisfy the spec first, naming what each lacks
spawn matrix                  # print the recorded capability matrix
spawn run job.toml            # provision, run, collect declared outputs, destroy
spawn session --workspace copy --root DIR    # one long-lived session driven by JSON lines on stdin
spawn adapter --config FILE   # turn queued GitHub Actions jobs into runners (needs a configuration file)
```

`spawn session` reaches a real project folder in one of two modes: `copy` (edits come back as a reviewed change
set that is applied by hash) or `bind` (the account edits the folder in place, and the grant is removed at close).

Not yet: the account pool is not wired to the CLI, `spawn adapter`'s run path is unmeasured end to end, and no
launcher other than the two above exists.
