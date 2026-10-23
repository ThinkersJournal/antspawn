# `actions/runner` provenance

What the hand-off will run inside an environment, and where it came from. Checked 2026-10-23.

| Item | Value |
|---|---|
| Project | `github.com/actions/runner` (GitHub, Inc.) |
| Release | `v2.338.1`, published 2026-10-22T14:14:46Z, tag commit `e78522a9a7b01c3bcd7ac0da6e3a9bb6cd513a76` |
| Archive | `actions-runner-win-x64-2.338.1.zip`, 104,210,469 bytes |
| SHA-256 | `478f533ac2c349040fbaea7e158f7f7d0235085cf35bc81f661af0c715207ff9` |
| Licence | MIT, "Copyright (c) 2019 GitHub"; text kept in `third_party/actions-runner/LICENSE` (read from the tag) |

## How the digest was verified

Three sources, read 2026-10-23: the release notes (`BEGIN SHA win-x64` marker, typed by the release workflow), the
release asset's `digest` field from the API (computed by GitHub on upload), and `sha256sum` of a copy downloaded from
the release URL. All three agree. The first two are both GitHub's; only the third is ours, and it can only show that
the download matches what GitHub serves, not that GitHub's build is the source at the tag. We do not rebuild it.

## What the zip does not carry

It has no LICENSE file (checked with `unzip -l`: `config.cmd`, `run.cmd`, `bin/Runner.Listener.exe`,
`bin/Runner.Worker.exe` and the .NET payload only). The licence therefore comes from the tag and is committed beside
this note. Any place that redistributes the archive's contents must ship that file with them.

## Rules

* `adapter::runner_pin` holds the pin; `verify` refuses any bytes whose size or digest differ. Nothing is unpacked
  or run unless it passed.
* Upgrading is a new pin, a new row here and a new digest check by this method, in its own PR.
* Not built: fetching, caching and unpacking the archive; starting the runner (see the open questions in HANDOFF).
