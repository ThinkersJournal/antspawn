# `spawn adapter`: three decisions before it is built (ROADMAP rows 14 and 23)

Docs only, written 2026-10-31 at `origin/main` 6ee9d40 (0.0.54) for the PM's ruling. **Nothing here was run.** Facts are read from this repo, from
ConfigurAnt and SecretAnt at their `origin/main` (not working trees), and from crates.io; each says which. What I could not read is marked UNVERIFIED.

**Ruled (PM, 2026-10-31):** (1) config = ConfigurAnt schema + TOML (B), falling back to plain TOML (A) only if the published `configurant` lacks `[[table]]`; (2) App key = the existing DPAPI/file providers behind `KeyProvider` (A), no dependency on SecretAnt; (3) launcher per group via a `RoutingDispatch`, one job at a time (A), measure before per-launcher busy. The two unverified points below were checked the same day; results are in place.

**Why this exists.** CodeRipper's 11 `reachability` findings (row 14) are true: nothing in `src/main.rs` or `src/bin/` calls the adapter, so `Watcher::tick`,
`SandboxDispatch`, `WsbBackend`, `ListenerTransport`, `AccountBackend` and the runner pin have callers only in tests. The fix is a command that runs the
watcher loop (`spawn adapter`, row 23), not a suppression (PM ruling 2026-10-31). Building it forces three choices that are not mine to make.

**What the command would do, in any option.** `spawn adapter run --config <file>` loads the configuration, builds a `HttpGithub` with a `KeyProvider`,
a `JobSource` and a `Dispatch`, and calls `Watcher::tick` on an interval. `--once` does one tick (for the proof runs). `--check` loads and validates
the configuration and reports, touching neither GitHub nor a launcher. The pieces already exist; only the choices below are open.

## Decision 1: where the configuration lives, and what reads it

**Today.** `AdapterConfig` (`src/adapter/github.rs`): strict TOML (`deny_unknown_fields`), fields `org`, `app_id`, `installation_id`, `key_path`,
`job_timeout_s`, and `[[group]]` rows (`name`, `repos`, `labels`, `launcher`). Cross-field rules are code (unique group names, unique label sets, exact
`owner/name` repos). No loader reads a file into it yet. The account side already keeps an admin-written `runner.toml` under `%ProgramData%\antspawn`.

**Facts about ConfigurAnt.** Published `configurant` 0.5.33 on crates.io (read 2026-10-31); `origin/main` is 0.5.39 (59a88db, unpublished). Its README
promises layering `default < file < environment < command line`, `explain`, named errors, a did-you-mean on unknown keys, schema migration, and "secrets:
environment only, never printed". Its `ValueType` has `Table` ("an array of tables, `[[name]]`") at 59a88db, so `[[group]]` fits; **I did not check that
0.5.33 already has `Table`** (UNVERIFIED), so building on it means depending on a version at least as new as the first one that does.

**Verified 2026-10-31 against the published crate, not `main`.** I downloaded `configurant-0.5.33.crate` from static.crates.io into an empty scratch folder and built a throwaway project with `configurant = "=0.5.33"` (crates.io, nothing from a working tree). A schema with a `[[table]]` named `group` (fields `name`, `repos`, `labels` and `launcher`, all `required`) and two `[[group]]` rows in a TOML file loaded as two rows with the right values; a misspelt key `launcer` in row 2 was refused as `UnknownKey { key: "group[1].launcer", suggestion: Some("launcher") }`; a missing required `launcher` was refused as `MissingRequired { key: "group[1].launcher" }`. So **the fallback to A is not needed**, and ConfigurAnt gives the row index and the did-you-mean for free. The probe is not committed. Not checked: the cross-row rules (unique label sets, exact `owner/name` repos) are not expressible in a schema, so `AdapterConfig::validate` stays as code on the loaded rows.
Portfolio roadmap: "every project needs configuration" is the reason ConfigurAnt exists; no ruling in `CIRESNAVE-EXPECTATIONS.md` makes adopting it
mandatory (6.14 names auth-framework, PersistAnt and SafeImage only).

| Option | What | For | Against |
|---|---|---|---|
| **A. Plain TOML at a fixed admin-owned path** | Keep `AdapterConfig`; add a loader: `%ProgramData%\antspawn\adapter.toml`, override with `--config`. Written by the machine owner, read by the watcher account, same pattern as `runner.toml`. | Nothing new to depend on; the strictness and tests exist; smallest change. | No `explain`, no env or command-line override, no migration path for later schema changes; every setting added later is hand-rolled. |
| **B. ConfigurAnt schema + the same TOML file** | Add `configurant.toml` for the adapter (scalars plus a `[[table]]` for groups); load through the `configurant` crate. `AdapterConfig::validate` keeps the cross-field rules. | The portfolio's own tool; `explain` and `check` for free; env override for tests; one place for migrations. A real first user of it in this repo (we already depend on `capant`). | New dependency; depends on `Table` being in a published version; two layers of validation to keep consistent; the TOML shape may need to follow the schema's rules. |
| **C. Hybrid** | A now; move to B when the adapter has settings that justify it. | No lock-in today. | A migration later; the file path and shape become something to keep stable. |

**Recommendation: B**, because the file will grow (poll interval, per-group account names, network notes) and a schema is cheaper before the first
user than after. If the PM would rather not depend on an unpublished feature, **A** is a safe start and the loader is the same size; the on-disk TOML
should be identical either way so the move is code-only. Either way the key itself is never in the file, only a reference (Decision 2).

## Decision 2: where the GitHub App key comes from

**What the key is.** The App's private key mints installation tokens, which mint the JIT configs. Whoever holds it can register runners for every repo the App
is installed on, so it is the most valuable secret this adapter touches. It must live outside every sandbox (`check_key_path` already refuses a path under one).

**Today (built, tested).** `KeyProvider` trait; `FileKeyProvider` (a PEM file under the watcher account) and `DpapiKeyProvider` (a DPAPI-protected blob, Windows
only, with a round-trip test run on this machine). Config holds a path, never the key.

**SecretAnt.** Design note draft 1 (2026-10-28, `origin/main` 5ed40fc): "Nothing is built", "no code exists". crates.io `secretant` is `0.0.0`, a name claim. Our
rule is crates.io dependencies only, so **it cannot be depended on yet**; ConfigurAnt's `docs/SECRET-SOURCES.md` already says the same and fixes only the
trait a SecretAnt adapter must meet (`SecretSource::fetch(reference) -> SecretValue`). The vault (`with-secret`) delivers an entry into a child process as an environment
variable, which ConfigurAnt's secret path already accepts (SECRET-SOURCES section 2: "needs no source"). Entry naming is `<SERVICE>_<PROJECT>_<PURPOSE>`, a
convention the PM proposed and CireSnave has not confirmed (EXPECTATIONS 6.12, second entry).

| Option | What | For | Against |
|---|---|---|---|
| **A. DPAPI (or file) behind `KeyProvider`** | Use what exists. Config: `key_path` plus `key_store = "dpapi" or "file"`. | Built and tested; no new dependency; key is never an env var or argument. | Windows-only DPAPI; the protected blob is machine-and-user bound, so a move is a re-wrap; rotation is manual; `CurrentUser` scope means any process of the watcher account can unwrap it (the same limit every tool here states). |
| **B. Vault entry delivered as an env var** | `with-secret` starts `spawn adapter run` with `GITHUB_ANTSPAWN_APP_KEY` set; a new `EnvKeyProvider` reads it. | Matches the portfolio's secret path and ConfigurAnt's "secrets from the environment only" rule; nothing on disk. | A multi-line PEM in an environment variable (the OS passes a 44-byte, 5-line value to a child process unchanged on this box, tested with PowerShell; the vault's own handling was not tried) is awkward; the process environment is readable by same-user processes; needs the vault to exist where the watcher runs. |
| **C. SecretAnt** | Wait; implement `KeyProvider` over it. | The portfolio's intended end state (requirement 1 of its design). | Does not exist; blocks the adapter on another project. |

**Recommendation: A for now, behind the existing trait**, with B added only if CireSnave wants the key out of a file on this machine; revisit when SecretAnt has a
published release. `KeyProvider` is already the seam, so the choice costs a provider, not a redesign. The config records which provider and where, never the value.

## Decision 3: sandbox or account, per group

**Today.** `RunnerGroup.launcher` is a string per group, and `docs/ADAPTER-GITHUB.md` says one org runner group per launcher. `Watcher` holds one `Dispatch`
(`busy()` is global: while one job runs, nothing is polled). Two `Dispatch` shapes exist: `SandboxDispatch<WsbBackend, ListenerTransport>` (a Windows Sandbox
VM per job) and `SandboxDispatch<AccountBackend<SchtasksTask>, PipeTransport>` (a restricted account per runner, from slices 21a to 21f). The account path
exists for host-resource tests (cpufish's AOCL); the sandbox path is the clean-room one.

**What is live.** Neither. Board 321 (Windows Sandbox feature plus a runner account) is unanswered and blocks the sandbox path; board 346 (the elevated
setup, then the three-run proof) blocks the account path. The shim, pipe, transports, `--fresh` copy and temp redirect are tested locally as the current user only.
(Board 346's point "the runner folder is not wiped between runs yet" is out of date as of 0.0.53 and 0.0.54; the real remaining gap is that a job can still
write the account's actual profile by absolute path.)

| Option | What | For | Against |
|---|---|---|---|
| **A. Launcher per group, one process, global busy** | A `RoutingDispatch` picks the backend from `group.launcher`; account groups also name their task and pipe. One job at a time across all groups, as `Watcher` does now. | Matches the config shape and the one-runner-group-per-launcher design; no change to `Watcher`; the safest first behaviour. | A long account job blocks sandbox jobs and the reverse; no parallelism even where two launchers could run side by side. |
| **B. Launcher per group, busy per launcher** | As A, but `Dispatch::busy` takes the group, and `tick` takes the oldest job whose launcher is free. | Uses the machine; a sandbox VM and an account runner can overlap. | Changes `Watcher` and its tests; a second runner on one host is a load and isolation question nobody has measured. |
| **C. One launcher per adapter process** | `launcher` moves to the top of the config; run one `spawn adapter` per launcher. | Simplest code; failures are isolated per process. | Two services to install and watch; a shared key to deliver to two processes; two polling loops against the same repos. |

**Recommendation: A**, and measure before B. Concretely: the `launcher` field decides the backend; an account group names its `task` and `pipe` (unique across
groups, validated); `spawn adapter --check` refuses a group whose launcher is not provisioned (the registry already knows), loudly. Revisit B when there is
a measured case where one launcher idles while another has a queue.

## What each ruling unblocks

| Ruling | Unblocks |
|---|---|
| 1 (config) | the loader and `--check`; the shape of the account groups' `task` and `pipe` fields |
| 2 (key) | which `KeyProvider` the command builds; whether an `EnvKeyProvider` is written |
| 3 (launcher) | `RoutingDispatch` and its group validation; whether `Watcher` changes |

None of them needs CireSnave's elevated step. Running the command against GitHub still needs the App installed (an owner action) and, for the account path,
board 346; so row 14 clears when row 23 merges, not when the adapter first works live.

## Questions I could not settle from the files

* ~~Does `configurant` 0.5.33 (published) support `[[table]]`?~~ **Yes**, verified above.
* ~~Does a multi-line PEM survive an environment variable?~~ **Between two processes on this box, yes.** Whether `with-secret` delivers it intact is not tried, and the ruling (A) does not need it.
* Is the `_RO`/entry-naming convention (6.12, second) confirmed for use in a docs and code example? It is "proposed, to confirm".
