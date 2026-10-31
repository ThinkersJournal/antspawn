# GitHub adapter (C2 design)

Decided by CireSnave 2026-10-22: *"Lets go with the GitHub app."* (board 241). Types and policy are in
`src/adapter/github.rs`; the real client is `src/adapter/github_client.rs` (0.0.13). What has run live is in
`docs/MEASUREMENTS.md`; everything else that needs GitHub is UNVERIFIED until it runs.

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

## The client (`github_client.rs`)

* `DpapiKeyProvider` reads the key as AgentLife's `github-app-setup.ps1` writes it: hex of a DPAPI user-scope blob
  (`ConvertFrom-SecureString`) whose plaintext is the PEM as UTF-16LE. Only the writing account decrypts it; the
  sandbox-root check from `FileKeyProvider` applies on construction and on every load. Windows only.
* `app_jwt` signs RS256 with `ring` (PKCS#1 or PKCS#8 PEM): `iat` = now - 60 s, `exp` = now + 540 s, `iss` = App id.
  A key that is not an RSA PEM is `KeyError::Malformed`, and the message never contains key text.
* `HttpGithub` (`ureq` + rustls) exchanges the JWT for an installation token (cached in memory for 3000 s, never
  written anywhere), then calls: list runner groups and a group's repositories (the `verify_group` input), `POST
  .../generate-jitconfig` (group id from the live list), list runners, delete a runner. Every call sets
  `Authorization`, `Accept`, `User-Agent` and `X-GitHub-Api-Version`.
* `classify` maps answers to `MintError`: 401 is `Auth`; 429, or 403 with `retry-after` or an exhausted quota, is
  `RateLimited` (wait taken from `retry-after`, else `x-ratelimit-reset`, else 60 s); 5xx and transport failures are
  `Unavailable`; any other status is `Rejected` carrying GitHub's `message` only (200 chars), never the body.
* A missing key stops before any request is made (test: a fake server sees zero connections).
* Group lookup reads the first 100 groups and 100 repositories; an org beyond that needs paging (not built).

## The watcher (`watcher.rs`, 0.0.18)

`Watcher::tick(now_s)`: if the dispatcher is busy, stop (nothing is polled). Otherwise poll every repo any group names (a failing
repo is reported and the others still run), forget remembered job ids GitHub no longer lists as queued (only after a complete
poll), take the oldest queued job not yet minted for that a group routes to, mint one runner named `antspawn-<run>-<job>`, hand
it to `Dispatch`, and stop: one job per tick. A minted job is remembered at once, even if the dispatch fails, so a broken
hand-off cannot mint a runner per tick (the unused ephemeral runner expires at GitHub). A failed mint is not remembered; the
`Minter` backoff spaces the retries. Tests use fakes and include a mutation check that dropping `seen` fails four of them.
(Since 0.0.65 the memory is `jobs`, with an attempt count; see "Retry" below.)

## Not yet built

A real `JobSource` (list queued runs and their jobs over REST) and a real `Dispatch` (start the runner inside the group's
launcher, which needs the JIT config delivered without a file or env: board 260). Live results so far: `docs/MEASUREMENTS.md`.

## Runner hand-off (board 260, built in 0.0.27 as far as the host)

The JIT config reaches the runner by an anonymous pipe to `spawn-jit-shim`, which sets `ACTIONS_RUNNER_INPUT_JITCONFIG` on the one
child it starts: never an argument, a file, or a variable of the host or the shim. `adapter::handoff::start_shim` is the host end.
The runner gets network only inside Windows Sandbox, so the shim is to be started there; that step waits on the sandbox starting.

## `spawn adapter`: exit codes, routing log, retry

| Code | Meaning |
|---|---|
| 0 | `--check` found no problem; continuous run stopped cleanly; `--once` polled and any runner it started ended |
| 1 | `--check` found problems (printed to stderr, one per line) |
| 2 | could not start: unreadable or invalid configuration, a group the router refuses (sandbox or unknown launcher), key provider setup failed, or a usage error (including `--once` with `--check`) |
| 3 | `--once` ended badly: a runner was minted but its dispatch failed, a runner ended in an error, or a runner was still running when the wait (job ceiling + 120 s) ran out |

Each dispatch is announced on stderr before it is made: `job <id> (<repo>): group `<g>` -> launcher `<l>``. No secret is in it.
`--check` is Windows-only for `account` groups: off Windows it reports a problem saying so and to run it on the Windows host.
### Retry (ROADMAP row 24, 0.0.65)

The job is still remembered the moment its runner is minted, so a broken hand-off cannot mint one per tick. What is new is that
`Dispatch::finished()` reports how each dispatched job ENDED (the real dispatcher runs the job on a background thread, so
`dispatch()` returning `Ok` says only that it was started). The watcher reads those at the start of every tick and of `--once`'s
final settle, and prints `job <id>: runner ended: ...` or `job <id>: the runner ended in an error: ...`.

A job is **minted for again** only when both hold: the end was an error with `nothing_ran`, and attempts are left. `nothing_ran`
means no runner could have taken the job: the environment never started (for `account`, `schtasks /Run` failed), or starting
succeeded but the config could not be delivered (the pipe never appeared, was busy or refused) and the backend ended the task.
An error after the config was delivered is **final**, because the runner may have taken work. Defaults: 3 attempts in all,
120 s between them (`RetryPolicy`, compiled in; not in the config schema, which the PM ruled on). The mint backoff still applies
on top. A job GitHub stops listing as queued while it waits (another runner took it) is forgotten, not minted for again.
`--once` never retries: it reports the error and exits 3.

A failure the dispatcher cannot classify (the default `finished()` reports nothing) is never retried. A `dispatch()` that
returns `Err` at once (already running, a launcher the adapter does not run) is final, as before.

How a failed account dispatch ends, measured on `ghr-spawn1`: `docs/MEASUREMENTS.md` 2026-10-31 "failed account dispatch".
