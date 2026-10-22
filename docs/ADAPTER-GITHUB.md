# GitHub adapter (C2 design)

Decided by CireSnave 2026-10-22: *"Lets go with the GitHub app."* (board 241). Types and policy are in
`src/adapter/github.rs`; this slice has **no network or signing code**, and nothing here is measured. Every
behaviour that needs GitHub or the elevated setup is UNVERIFIED until it runs.

## Flow

1. A watcher sees a queued job (`workflow_job` event or poll) for a repo and its `runs-on` labels.
2. `AdapterConfig::route(repo, labels)` picks exactly one runner group: the repo must be named in the group and
   every group label must be on the job. Zero or several matches: no route, the job is left alone.
3. `Minter::mint` checks the org runner group live (`verify_group`), then calls
   `POST /orgs/{org}/actions/runners/generate-jitconfig` with a unique runner name and the group's labels. The
   result is an ephemeral runner config that serves one job.
4. The JIT config is handed to the group's launcher (`account` or `windows-sandbox`), which starts the runner
   inside a fresh environment and destroys it afterwards. The config is a `Secret`: never logged, never written
   to env or a file by AntSpawn (the runner hand-off is the only reader, via `expose`).

## The App key

* Lives **outside** every sandbox, under the watcher account (AgentLife's C1 helper creates it). Config holds a
  **path** (`key_path`), never the key.
* Read only through the `KeyProvider` trait; tests inject a fake. `FileKeyProvider` is the file-backed one.
* **Known-bad control:** `check_key_path` refuses a path that is relative or lies under any sandbox root (mount
  host paths, work dir, spool, results), by lexical normalisation (`..`, case) and by resolved path (links). It
  runs at construction and again on every load. Test: `key_inside_the_sandbox_is_refused` (with a positive
  control that an outside path passes).
* Errors name the path, never contents. `Secret` has a redacted `Debug` and no `Display`, `Clone` or `Serialize`.
* Signing the App JWT and exchanging it for an installation token belong to the real `GithubApi` client (not in
  this slice); it receives the key only from a `KeyProvider`.

## Labels and runner groups

One org-level runner group per launcher. A job selects it with `runs-on: [self-hosted, antspawn, <launcher>]`;
the group's labels are what the config lists. Config rules (all tested): group names unique; label sets unique
(two groups sharing one set would route ambiguously); labels non-empty; every repo exactly `<org>/<name>`, no
wildcard; a group names its launcher.

**Restricted to named repos.** Before every mint, `verify_group` reads the live group and requires
`visibility = selected` and a repo list *equal* to the config's: extra repos are refused (someone widened the
group), missing ones are reported. Public repos need the group's public-repository switch on at GitHub; that is
a setting we do not manage and the check does not treat as an error, but the repo list must still be exact.

## Job timeout

`job_timeout_s` (config, 1 to 21600) is the ceiling for every job. `clamp_time(spec.limits.time_s)` grants the
spec's request up to that ceiling and defaults to it. It becomes the job's `limits.time_s`, which the launcher
enforces. Workflows should also set `timeout-minutes` so GitHub marks the job failed; the adapter does not
depend on it.

## When minting fails

The GitHub job simply **stays queued**; the adapter touches nothing else. `mint` makes at most one attempt per
call and records the next allowed time, so there is no retry storm:

| Error | Gap before the next attempt | Operator needed |
|---|---|---|
| `Unavailable` (5xx, network) | 5 s, doubling per consecutive failure, capped at 900 s | no |
| `RateLimited { retry_after_s }` | at least `retry_after_s` | no |
| `Key`, `Auth`, `Rejected`, `Group` | the cap (900 s) | **yes**: reported, not retried faster |

A call inside the cool-down returns `StayQueued` without reaching GitHub (test counts API calls). Success resets
the counter. Time is passed in, never read, so tests are deterministic. Backoff values are defaults, not
measurements.

## Not in this slice

The HTTP client, JWT signing, the watcher loop, the hand-off of a JIT config into the runner, and any
measurement (mint latency, runner registration time, cleanup of a runner that never took a job). Those follow
the C1 key helper and the elevated setup; until then the matrix rows stay as they are.
