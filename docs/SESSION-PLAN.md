<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Session slice plan: A1 session, A3 pool, A2 both workspace modes

Tasked by the PM 2026-11-06 (`[TASK] sandbox-sessions-A1-A2-A3`). Builds on `docs/SESSION-OPTIONS.md` (sizing, `cdcd1c8`); agent-loop asks are
`agent-loop` `docs/SANDBOX.md` section 5.5. **Plan only: nothing here is built.** Sizes are estimates, not measurements. Every capability stays off
in `matrix/` until a probe with a known-bad control says `yes`.

**Ruling (CireSnave, via the PM, board 373, verbatim):** *"373: B1 and B2.  I'd like the ability to do either one.  Lets build both and make it easy to choose one or the other."*
Deletes: write-back refuses them unless approved per file (PM tasking). Protected paths `.git`, `.github`, `.claude`, `CLAUDE.md`, hooks: deny entries if a
probe shows deny beats the grant (B1); a path denylist in the policy filter (B2).

## Decisions this plan makes (change them before slice 1, not after)

1. **Account only.** `windows-sandbox` stays Tier J (job per call): one VM at a time (`CO_E_APPSINGLEUSE`, measured) rules out A3 and `wsb exec` reuse is unmeasured.
2. **Session host over a named pipe** (SESSION-OPTIONS A1 way 2). `serve` starts `spawn-runner session <sid>` inside a kill-on-close Job Object; the host
   creates `\\.\pipe\antspawn-session-<sid>-<nonce>` with a DACL granting the launcher SID only (the `spawn-jit-shim --pipe --allow-sid` mechanism). **The DACL rests on a check, not on the spool ACLs alone:** `session.toml` is launcher-writable, so the host reads the OS-reported owner of the spool entry and refuses a `launcher_sid` that is not that owner (a spool several users can write into would otherwise let user A mint a session admitting user B; the 128-bit nonce is the second barrier, not the first). Unmeasured with a real second user.
3. **Frames:** 4-byte little-endian length then JSON, hard cap 1 MiB per frame, one client per pipe instance, remote clients rejected. Requests `exec{argv,cwd,env,time_s}`, `close`;
   later `cancel` (A4) and streamed `output` frames (A5) fit the same envelope, so slice 2 reserves their tags and refuses them as unknown until built.
4. **Owner gone = pipe closed.** The launcher holds the connection for the session's life; a disconnect (including a killed launcher, whose handles the OS closes) ends the
   session: host exits, Job Object kills the tree, work dir and the account's profile temp are wiped. Not measured; slice 5 measures it.
5. **One setting chooses the workspace:** `workspace = "bind" | "copy"` on `open_session` (and `spawn session open --workspace ...`). Default `copy` (the reviewed one).
6. **Session work dir persists across execs** and is wiped at close. `clean-slate-next-run` does not describe it: new capabilities `session`, `clean-slate-next-session`,
   `no-read-other-session` (agent-loop A6).

## Slices, in build order (one PR each, `[READY]` per PR)

### A1: session on `account`
| # | Slice | Proof it bites |
|---|---|---|
| S1 | `Session` trait beside `Launcher` (`open_session`, `exec`, `close`; `close` also on `Drop`), `session` capability, registry refuses with the unmet need named, `naive` session impl | `naive` must FAIL the persistence-after-close control |
| S2 | `session_proto`: framing, size cap, request/response types; pure, runs on every CI leg | oversized frame, truncated frame, bad JSON, unknown tag, all refused |
| S3 | `spawn-runner session` host + `serve` hook + `Account::open_session`; pipe DACL | runner-only: a second client, a wrong SID and a remote client are refused (UNVERIFIED in CI, run here, recorded in MEASUREMENTS) |
| S4 | Close and abandon: close wipes work dir and the account's profile temp; owner disconnect or crash does the same; orphan reaping on next open | state persists across execs; nothing after `close`; a killed owner ends the session |
| S5 | `spawn session` CLI verbs, result contract per exec, `session` capability row `yes` for `account` | matrix diff gate |

### A3: pool of N sessions (after S5)
| # | Slice | Proof |
|---|---|---|
| P1 | `Layout` and `AccountLauncher` take a named account (today fixed `antspawn-run` / `AntSpawnRunner`); pool = the set the setup registered | single-account behaviour unchanged (existing tests) |
| P2 | Lease allocator: a session takes a free account, released at close, reclaimed when its owner is gone | two opens get two accounts; a third is refused naming "pool exhausted" |
| P3 | `concurrent` probe and `no-read-other-session`: two sessions at once, wall time not serial, each denied the other's canary | known-bad control: both sessions forced onto one account must FAIL |
| P4 | Fold the firewall block into the pool setup (`runner-account-setup.ps1` deliberately leaves network on, board 341); one elevated paste per account | **needs CireSnave's elevated paste per account, via the PM**; N fixed at setup |

### A2: both workspace modes (alongside; B2 can start after S3)
| # | Slice | Proof |
|---|---|---|
| W0 | **Measure first:** does an explicit Modify grant on a subtree beat the inherited `C:\Projects` deny, and does an explicit deny on `.git` beat that grant? Throwaway tree, `antspawn-run` | the result decides W4's shape; recorded with date and ref |
| W1 | `copy` mode: copy in with a hash manifest; diff at `sync` and `close` run by the launcher user, never the account | edit appears in the diff; untouched files do not |
| W2 | Apply with conflict detection (real tree changed since the manifest: reported, not overwritten) | conflict control |
| W3 | Policy filter: protected-path denylist, size cap, reparse points and symlinks refused, **deletes refused unless approved per file** (`sync` takes an approved-deletes list; the approver is the harness's, not ours) | denied path, junction, unapproved delete each refused |
| W4 | `bind` mode: grant at open, revoke at close, a grant journal so a crashed launcher's grants are reclaimed; deny entries on protected paths if W0 shows they win | sibling project stays denied; protected path write fails; journal reclaim after a kill |
| W5 | The one setting wired through `open_session`; `bind` and `copy` pass the same edit-in-place probe | same probe, both modes |

Order: S1-S5, then P1-P3 and W0-W3 interleaved (they are independent), then W4-W5, P4 last (waits on the paste).

### `copy` mode on the `spawn session` lines: `changes` and `apply` (agent-loop's ask, 2026-11-12)
Two ops answered by the CLI itself (the pipe host refuses them; `bind` and plain sessions answer `error`). Output is one JSON line per request, like every other op.

- `{"op":"changes","id":N}` fetches what the session changed and stages it. Reply: `{"op":"changes","id":N,"added":[{"path","size","sha256"}],"modified":[same],"deleted":["p"],"later_step":["p"],"refused":[{"path","why"}],"set_hash":"<64 hex>"}`. `refused` is what policy refuses unless the apply names it by exact path (protected paths, deletes, the size cap); `later_step` is what runs on a later step outside the sandbox (`build.rs`, `Makefile`, `justfile`, `package.json`). Another `changes` replaces the staging.
- `{"op":"apply","id":N,"set_hash":"..","deletes":["p"],"protected":["p"]}` writes the staged set into the real project in the spawn process, as the invoking user, all or nothing. Reply: `{"op":"applied","id":N,"set_hash","applied":["p"],"refused":[{"path","why"}],"conflicts":[{"path","why"}]}`. `refused` or `conflicts` non-empty means nothing was written and the set is still staged. A `set_hash` that is not the staged set's, or nothing staged, is an `error`, never a partial apply. A real file that no longer matches the base is a conflict, never overwritten. The approval (`deletes`, `protected`) is the harness's, never this crate's.
- After a clean apply the applied files become the new base, so the next `changes` reports only what happened since.
- **`set_hash` byte format:** SHA-256, lowercase hex, over the concatenation of: one line `A\t<path>\t<size>\t<sha256>\n` per added file in path order, then one `M\t<path>\t<size>\t<sha256>\n` per modified file in path order, then one `D\t<path>\n` per deleted path in path order. `<size>` is decimal, `<sha256>` is the file's lowercase hex SHA-256, paths are the `/`-separated relative paths as listed. The empty set hashes the empty string. A test (`set_hash_is_the_documented_byte_format`) pins one known vector.

## Costs and risks stated now
- A session pins its account's runner for its life (one task, one job): that is why A3 follows A1 rather than preceding it.
- `bind` widens the account's reach to the whole granted tree; any host-side run of what it wrote (tests, hooks, CI) is an escape. That is the user's ruled choice; the default is `copy`.
- `ram_mb` is per job and `cpu` is refused (A7): one session can starve another.
- The deployed `spawn-runner` must be redeployed for S3 (as for `account` streams today). **Unknown whether that needs an elevated step**; S3's PR will say.
- Copy-in time on a real project, session exec latency, the ACL ordering, and the disconnect-ends-session behaviour are all **unmeasured**.

## What I need from the PM
- Go on S1 now (no elevation needed). W0 needs only the already-provisioned `antspawn-run`.
- Before P4: CireSnave's choice of N.
- Proof for C2 against the org App is a separate reply.
