# Changelog

## 0.0.127 — account pool sized to four accounts

CireSnave's ruling on board 420 (2026-11-12, "420: B.") fixes the pool at four accounts. `docs/ACCOUNT-POOL-SETUP.md` now gives the run and undo lines as `-Count 4` and the four-member `ANTSPAWN_POOL`, and `docs/SESSION-PLAN.md` records N = 4. The script already accepts 1-8, so no code changes. Not measured: a `-Count 4` dry run, any `-Apply`, and the pool test beyond two concurrent sessions (it exercises two of the four accounts, plus its two known-bad controls).

## 0.0.126 — `spawn session` copy mode returns and applies the change set

`spawn session --workspace copy` now answers two more lines: `{"op":"changes","id":N}` returns the added, modified and deleted files with sizes and hashes, the `later_step` paths, what policy refuses without an exact-path approval, and a `set_hash`; `{"op":"apply","id":N,"set_hash":..,"deletes":[..],"protected":[..]}` writes exactly that staged set into the real project as the invoking user, all or nothing (a different hash, nothing staged, a refusal or a conflict writes nothing). A clean apply moves the base, so the next `changes` reports only what came after. `bind`, plain sessions and the pipe host refuse both ops. The `set_hash` byte format is in `docs/SESSION-PLAN.md`. Verified by unit tests (a known hash vector, the reviewed-set binding, the moved base, the JSON fields); not run against the real account, which waits on the runner swap.

## 0.0.125 — `spawn-runner --root` and a tree per pool account (A3 P2b)

`spawn-runner [--root <dir>] serve|exec|session`: the flag sets the layout root (over `ANTSPAWN_ROOT` and the ProgramData default), and the `exec`/`session` children `serve` starts inherit it. `AccountPool` now gives each leased account its own tree, `<pool root>/<account>`, instead of one shared layout, so no directory is shared between pool accounts. Needed by the pool paste (`docs/proof/PASTE-pool-accounts.ps1`, whose tasks run `spawn-runner --root <tree> serve`); the single-account setup and its task are unchanged. Verified by `tests/runner.rs` (the flag beats a decoy `ANTSPAWN_ROOT`, the job answers in that tree only, a bare `--root` exits 2) and a pool unit test; a real account is not exercised.

## 0.0.124 — Account pool elevated paste and undo (A3 P4)

`docs/proof/PASTE-pool-accounts.ps1` and `docs/ACCOUNT-POOL-SETUP.md`: creates N restricted accounts, each with its own directory tree, task (`spawn-runner.exe --root <tree> serve`), batch-logon right, outbound firewall block and a deny on `C:\Projects`, hash-checked against the deployed runner, with a read-back per step and a documented `-Undo`. Changes no existing ACL. Dry run only: parsed under PowerShell 5.1 and 7, dry-run and undo-dry-run output read, wrong hash refused; `-Apply` has not been run. Needs the `--root` runner flag (next slice).

## 0.0.123 — Probes for `concurrent` and `no-read-other-session` (A3 P3)

`Probes::violates_concurrent` and `Probes::violates_no_read_other_session` take an opener for one more session, and `spawn-probe cwd` reports a session's directory. The isolation probe has a positive control (the writing session reads its own file) and treats any answer but the probe's own yes/no as inconclusive. Proven able to fail: `naive` (one shared directory) fails `no-read-other-session`; a pool of one fails `concurrent`; a per-session-directory fake passes both (probe logic only). `tests/account_pool.rs` runs the same on real pool accounts with two known-bad controls (pool of one; two sessions forced onto one account) and prints UNVERIFIED until `ANTSPAWN_POOL` and `ANTSPAWN_POOL_PROBE` are set after P4. No matrix row changes: both stay `no` until the real run passes.

## 0.0.122 — Account pool lease allocator (A3 P2)

New `pool` module: `Pool` (ordered `AccountIdentity` list), `Lease`, `PoolError` and `AccountPool::open_session_in`, which leases a free account, opens the account launcher's session on it, and frees it when the session closes or drops. The lease is `<root>/leases/<account>.lease` opened with share mode 0 and delete-on-close, so the operating system releases it when the owner dies (no pid record, no pid reuse, no reclaim race). A third open on a two-account pool is refused with "pool exhausted". Windows-only; elsewhere every acquire is refused loudly. Not yet wired to the CLI or the registry, and a workspace `bind` cannot yet grant the leased account (`LeasedSession::principal` names it; P3 and later). Verified by unit tests: distinct leases, exhaustion then reuse of the freed account, a killed owner process releasing its lease.

## 0.0.121 — Account launcher takes a named account and task (A3 P1)

`launchers::account::AccountIdentity { account, task }` plus `Account::with_identity` / `identity()`: the account name (bind-grant principal) and the scheduled task the launcher triggers are no longer fixed to `antspawn-run` / `AntSpawnRunner`. `Account::new` and `Default` use the old constants, so single-account behaviour is unchanged. First slice of the account pool (P2 lease allocator, P3 isolation probes, P4 elevated setup per account follow). Verified by `cargo clippy --all-targets` and a unit test that the default identity equals the constants and a named one reaches `bind_principal`.

## 0.0.120 — Workspace modes W5 end: `spawn session --workspace copy|bind --root DIR`

`spawn session` takes `--workspace copy|bind` and `--root DIR` (`--workspace` needs `--root`; with `--root` alone the mode is `copy`, the reviewed default; an unknown mode is refused, never defaulted). It goes through `Registry::open_workspace_session`, so the same matrix check applies and a launcher that is not certified for `session` is refused before anything opens, with no output on stdout. On success the first stdout line is `{"op":"opened","workspace":"copy-back"|"bind","root":...}`, so the caller learns which mode it got; after that the line protocol is unchanged. Without `--root` nothing changes. A test runs the same edit command in both modes (windows, `naive` standing in for the account): in `bind` the file is in the real tree at once, in `copy` only after the reviewed `apply`. Not done: reaching `changes`/`apply` of a copy session through the CLI lines (the library has them; the line protocol does not yet), and the matrix rows for the two modes, which stay absent (off) until the account-backed run after the runner redeploy (board 416). Unmeasured: both modes as the real account.

## 0.0.119 — Workspace modes W4 part 2 (bind through the session)

Workspace modes, slice W4 part 2 : `bind` wired through the session. `Launcher` gains `open_session_in(Some(root))` (default: refused, so a bind never quietly becomes something else) and `bind_principal`; `account` passes the root in `session.toml` (new optional `root`; an older host rejects the unknown field, so a bind against an old runner fails loudly instead of running in an empty folder) and names `antspawn-run`; `naive` runs in the folder as the invoking user (the unisolated control, nothing granted). The host runs commands in the root after `session_host::bind_root` accepts it (absolute drive path, no `.`/`..`, an existing directory, not overlapping the runner's own spool, results or work dirs) and never wipes it. `workspace_session::open_bound` grants first (denies then Modify), then opens the session, and revokes the grant if the open fails; `WorkspaceSession::close` ends the session, then revokes and proves removal, and reports a failure of either. `changes()` is an error in bind (edits are in place). `Registry::open_workspace_session` serves `Mode::Bind`. Not wired to `spawn session open` yet (W5 end). **UNVERIFIED live**: needs the spawn-runner redeployed (board 416); nothing here ran as the real account. Unmeasured: whether the account's `.git` read/`git status` works inside a real bind session (W0 measured the ACL, not a git run), cwd handling when the root is on another drive.

## 0.0.118

- Tooling: adopt the changelog-fragment kit (`tools/stamp-version.py`, `tools/check-fragment.py`, `.github/workflows/changelog-fragment.yml`, `changelog.d/README.md`). From the next PR on, a PR carries one `changelog.d/` fragment and no version edit; the PM stamps at merge. Dry run with a throwaway fragment listed Cargo.toml, Cargo.lock and CHANGELOG.md. Provisional (PM allocates).

## 0.0.116

- Docs: `docs/REDEPLOY-RUNNER.md` and `docs/proof/PASTE-redeploy-runner.ps1`, the procedure for swapping `C:\Program Files\antspawn\spawn-runner.exe` (CI artifact, hash-checked, elevated swap, read back, rollback). Only the swap is elevated; fetching the artifact and the read-back are not. Provisional (PM allocates).

## 0.0.115

- Workspace modes, slice W4 part 1 (`bind`): the `bind` module, the launcher-side grant engine. `BindGrant::open` denies every write (exact rights `WD,AD,WEA,WA,D,DC`, no SYNCHRONIZE, as W0 measured) on each protected path that exists in the project, THEN grants the account Modify on the root, so a protected path is never writable; a link anywhere in the tree is refused before any ACL change. A journal entry (`%LOCALAPPDATA%\antspawn\grants`, launcher-user-owned) is written before the first change; `revoke` removes the account's entries and READS BACK that it is no longer named on the root or any denied path, failing loudly (and keeping the journal) if not. `reclaim` undoes the binds whose launcher pid is gone; two sessions on one root, or on nested roots, are refused while the first owner lives, and a dead owner's bind is reclaimed by the next open. `new_protected_paths` reports a protected path the account made after open (nothing could deny it). ACL calls go through an `Acl` trait: `Icacls` is real, tests use a recording fake, and one test runs the real `icacls` on a throwaway tree with the built-in Guests group standing in for the account (grant, exact-rights deny and read-back removal all observed). Not wired yet: the session host's cwd, `session.toml` `root`, `open_workspace_session` for bind, and the CLI (next part, needs the spawn-runner redeployed). Unmeasured: `icacls /T` cost on a large tree, pid reuse (a recycled pid keeps a dead launcher's grant until the next reclaim finds it dead), the account's behaviour against these ACLs end to end. Provisional (PM allocates).

## 0.0.114

- Workspace modes, slice W5 part 2 (stacked on #102): the launcher side of copy-back. `Session` gains `put`, `get` and `list` (default: refused), served by the account session over the pipe and by `naive` straight from its directory. `WorkspaceSession` wraps any session: on open it walks the project and `put`s every file in 512 KiB chunks, building the BASE manifest from the bytes it actually read and sent (the host's hashes are never trusted for the base; a reparse point stops the copy-in; per-file and total caps apply; a failed open closes the session it opened). `changes()` pages `list`, fetches only differing files into a launcher-side staging directory (removed on drop), re-hashes them locally, and diffs against the base; `apply(&staged, &Approvals)` writes them through `workspace::apply` as the user. AntSpawn never approves: refusals (deletes, protected paths) need the harness's `Approvals`. The host is untrusted: an unsafe, repeated or oversize path in its listing is an error before any fetch, and a file that outgrows the cap while read is refused. `Registry::open_workspace_session` runs the same matrix check as `open_session`; `bind` is refused until W4. Not yet wired to `spawn session open`. Tested against a directory-backed session and hostile fakes; the real account pipe path is UNVERIFIED until the spawn-runner is redeployed and run. Provisional (PM allocates).

## 0.0.113

- Workspace modes, slice W5 part 1 (stacked on #101): the session protocol gains `put`, `get` and `list` and the host serves them (`session_files::WorkFiles`). The account's work dir is readable and writable by the account only, so the launcher's user cannot copy a project in or diff it out directly (and the setup script is not changed to allow it, which would need CireSnave's elevated paste); a project goes in, and changes come out, as frames through the session pipe instead. A file goes in chunks of at most 512 KiB (base64 inside the 1 MiB frame cap); offset 0 creates or truncates, any other offset must equal the file's current length, so a lost or repeated chunk is an error, never a gap; a per-file cap applies. Paths must be relative, `/`-separated, without `..`, drive or empty parts, and a symlink or junction a command made in the workspace is never followed (`list` names it as an error rather than skipping it). `list` returns path, size and SHA-256, sorted, in pages of 2000. `serve_session` takes the host's `WorkFiles`. A refusal is an `error` frame and the session goes on. Nothing uses these on the launcher side yet (the next part), and `spawn session` refuses them. Unmeasured: hashing and transfer time for a large project through the pipe.

## 0.0.112

- Workspace modes, slices W1-W3 (agent-loop ask A2, copy-back): `workspace` module, pure file logic that runs as the launcher's user, never the sandbox account. `Mode` (`copy-back` default, `bind`; anything else refused, no "auto"), `copy_in` (working copy plus a SHA-256 base `Manifest`; a symlink or Windows junction is an error, never followed; per-file and total size caps), `diff` (added, modified, deleted), `check`/`Policy` (protected paths `.git`, `.github`, `.claude`, `.githooks`, `.husky`, `CLAUDE.md`, `.cargo/config.toml` refused, deletes refused, both unless the harness's `Approvals` names that exact file; build scripts listed via `ChangeSet::later_step`, not refused) and `apply` (all or nothing: a real file changed since the manifest, an added file that now exists, a vanished file, a working-copy file changed after the diff, or a real-tree ancestor that became a link is a `Conflict` and nothing is written). AntSpawn holds no approval of its own. Not wired into `open_session` yet (W5), and `bind` (W4) is not built. Tested on temp trees with controls (the no-edit diff is empty, an approved path goes through); the junction tests ran here (no SKIPPED). Unmeasured: copy-in time on a large tree, and Windows reparse kinds other than junctions and symlinks.

## 0.0.111

- Windows Sandbox broker (Track 2): example files and deploy docs. `examples/sandbox/` ships `sandbox-broker.example.toml`, `default.wsb` (everything off, no folder) and `staging.wsb` (the same plus one read-only mapped folder; a placeholder path), and `docs/SANDBOX-BROKER.md` gains "Deploying the sandbox broker" (templates in a broker-only-writable directory, `--check`, expected behaviour, what is undecided). 3 new tests in `tests/sandbox_broker_check.rs`: the real binary's `--check` accepts the shipped config and offers both profiles; the real `WsbDriver` starts each shipped template from disk and hands a fake `wsb` exactly its text, with a known-bad control (the same file with a leading comment is refused and never reaches `wsb`: the driver wants `<Configuration` first, which is why the shipped `.wsb` files carry no header comment); the shipped templates keep network, GPU and clipboard off, protected client on, nothing writable, no logon command. **UNVERIFIED, never run:** a real `wsb start -c` with either file (and whether `wsb` accepts them as written), and how a client gets work in and results out of a no-network sandbox. Provisional (PM allocates).

## 0.0.110

- Windows Sandbox broker (Track 2), the binary and its configuration: `sandbox_config::SandboxConfig` (a checked TOML file: `pipe`, `allow_sids`, `[[profiles]]` of `name` + the absolute path of a fixed `.wsb` template ending `.wsb`, optional `[limits]` `creates_per_minute` and `max_lifetime_s`; unknown keys, `max_total` included, are errors; the offered profiles are exactly the rows) and `spawn-sandbox-broker --config FILE [--check]`. `--check` prints what is offered and starts nothing; serving wires `Listener` (SID read from the OS) to `serve_connection` and one shared `SandboxBroker<WsbDriver<WsbExe>>` locked per request, a 5-second tick thread, at most 8 connections. 3 unit tests (good config offers exactly its profiles; bad pipe, SID, name, relative, `..` and non-`.wsb` template and unknown key refused naming the field; repeated profile, short lifetime, `max_total`) and 3 tests of the real binary's `--check` on real files. **UNVERIFIED, never run:** serving against a real `wsb`, a real second account or a real pipe client. Template files are the operator's to write (none shipped); `docs/SANDBOX-BROKER.md` still has to say `create` does not start a VM. Provisional (PM allocates).

## 0.0.109

- Windows Sandbox broker (Track 2), the real driver: `wsb_driver::WsbDriver` implements `SandboxDriver` over a `Wsb` runner (the real one is `WsbExe`, the package's `wsb.exe`). A profile names a **fixed template** (the operator's absolute `.wsb` path, at most 64 KiB, must start with `<Configuration`), read at start and passed to `wsb start -c`; the sandbox id is the one `wsb start` printed (`sandbox_id_in`, now public). `stop` is idempotent and **reads back**: it lists, returns success if the id is not listed (it never calls `wsb stop` on an id that is gone), stops, lists again, and reports an error if the id is still listed, so the broker keeps its single slot held and retries. An id that is not hex-and-dashes starting with a hex digit is refused before any `wsb` call, and a sandbox that is not the one asked for (another owner's) is never touched. 7 tests against a fake `wsb` (template passed verbatim and id returned; unlisted profile, empty, non-configuration, oversized and missing templates never reach `wsb`; no id printed; list-stop-list; absent id is success without a stop; a stop that sticks is an error; odd ids refused and another owner's sandbox left alone). **UNVERIFIED, never run against a real `wsb`:** the format of `wsb list` beyond one id per line (the launcher's `running_sandboxes` assumes the same), what `wsb stop` prints or does for an unknown id (not relied on), and how long a stop takes to show in the list (the read-back could see it too early and report a false failure, which only causes a retry). Not built: the broker binary and pipe wiring for this track, and the template files. Provisional (PM allocates).

## 0.0.108

- Windows Sandbox broker (Track 2), core: `sandbox_broker::SandboxBroker` behind a `SandboxDriver` (`start(profile) -> wsb id`, `stop(id)`), the sibling of the VM broker's core. Requests are validated by the existing `broker_proto` (so a host path, an unoffered or malformed profile is refused before the driver is called); **one sandbox at a time** (Windows Sandbox allows a single instance), held until the driver confirms it is stopped, so a failed stop is retried by `tick` and nothing else starts meanwhile; a sandbox belongs to the connection that started it (disconnect, lifetime expiry and a failed start clean up; only the owner may stop it). New wire answer `Response::SandboxStarted { vm }`; `Client::sandbox_start`. 8 tests against a fake driver (driver gets only the offered profile; 4 bad requests never reach it; second account refused while one runs; disconnect frees the slot; stranger cannot stop it; failed start holds nothing; failed stop keeps the slot and is retried; lifetime expiry; VM verbs refused, list shows only your own). **No sweep at start, on purpose:** sandboxes carry no name of ours and `wsb list` shows every owner's, including the user's own work, so the broker never stops one it did not start; a crashed broker's sandbox lingers until stopped by hand. **Not built, not run:** the real `wsb` driver, the pipe wiring and a broker binary for this track, and SB2 (a real run for a restricted account). Provisional (PM allocates).

## 0.0.107

- Broker slice V4, first part: `broker_client`, the client side of the VM broker. `Client<S>` over any byte stream with the five verbs a test needs (`create`, `start`, `stop`, `delete`, `list`); a broker error is `Err(message)` and a reply of the wrong kind is an error, never a guess; `connect(name)` opens the broker's pipe, retrying for up to 5 s while every instance is busy. The connection owns its VMs: dropping the client closes the stream and the broker destroys them, so a client that wants a VM to survive a stop and a start keeps the connection open across both. 4 integration tests (tests/broker_client.rs) on the **real pipe, real framing and real broker core with a fake driver**: the five verbs and a stop-then-start leaving the same VM listed; a client that vanishes loses its VM; a refused create (unoffered image, bad key) and a delete of an unknown VM arrive as errors and never reach the driver; a bad pipe name and a missing broker are errors. **Not shown:** anything against real Hyper-V, a client from another account, and the PersistAnt power-cut itself (the first real user). Note: `Request::Stop` has no `force` field because the driver's stop is always `Stop-VM -TurnOff -Force` (a power-off; script text only, unrun), which is what the power-cut needs; a graceful stop does not exist. Also: the design's "create returns when the guest is ready" is not what the code does: `create` does NOT start the VM and the driver does not wait for the guest, so the caller calls `start`, then `wait_ssh(address, timeout)` (new: polls TCP 22, accepts only 10.77.0.0/16; `wait_port` is the general form, tested on loopback including the timeout; 2 unit tests); the design text should say so. Based on 0.0.105; provisional (PM allocates; #95 is 0.0.106).

## 0.0.106

- Broker slice V3, seventh part: the `spawn-broker` binary, and `serve_connection` reshaped so one broker can be shared. `serve_connection(r, w, handle, finish)` now takes a request handler and a `finish` closure (run on every exit path, where the connection's VMs are destroyed), so the binary locks the shared broker **per request**, never across a blocking read; `serve_owned` keeps the old single-owner form for tests. `spawn-broker --config FILE --check` reads and checks the config and prints what it offers, touching no Hyper-V (3 tests run the real binary on real files: good, bad naming the field, missing file and bad usage). `spawn-broker --config FILE` serves: sweeps leftover `antspawn-` VMs, creates the pipe, accepts clients (SID read from the OS), one thread per connection capped at 32 (further clients are dropped at accept), a 5-second thread that runs `tick` (lifetime expiry, delete retries). A poisoned lock stops the process (the next start sweeps). No shutdown command: stopping the process leaves its VMs until the next start. **UNVERIFIED, never run:** serving against real Hyper-V, a second account, or a guest; the idle-connection cap is a number chosen, not measured; a client that connects and sends nothing holds a thread until it leaves. Version provisional (PM allocates).

## 0.0.105

- Broker slice V3, sixth part: `broker_config::BrokerConfig::from_toml`, the broker's configuration file, checked as a whole: pipe name, `allow_sids` (1..=16, strict SID shape), `hyperv` (absolute plain-ASCII `base_dir` and `vm_dir`, which must differ, and `base_hashes`: the offered images are exactly its keys, each 64 hex digits), `profiles` (1..=16, unique plain names, memory 256..=65536 MB, vcpus 1..=16, disk 1..=512 GB), `slots` (1..=32, unique switch names, host and guest addresses inside 10.77.0.0/16 and never used twice) and an optional `limits` table that overrides only what it names (max_total 1..=64, per-account no higher than the total, lifetime 60..=86400 s). Unknown keys are errors; messages name the field and never echo a value. 4 tests (a good file; 18 known-bad files each refused naming its field, including an SDDL-injection SID and a `..` path; limits override; repeated profile or switch). **Not shown:** that the paths and switches exist, or that the pinned hashes match anything on disk (the driver checks the hash at create). Still not built: the broker binary's main loop. Stacked on #93 (0.0.104). Version provisional (PM allocates).

## 0.0.104

- Broker slice V3, fifth part: `broker_pipe::Listener`, the broker's multi-client pipe. `new(name, allow_sids)` makes the first instance of the named pipe `<name>` with a protected DACL of one ACE per allowed SID, remote clients rejected; the name and every SID are shape-checked (`session_host::valid_sid`, now shared with `session.toml`). `accept()` returns the connected pipe and the client's SID **read from the OS** (the client process id from the pipe, then that process's token user; impersonation was tried first and the OS refuses it before the server has read a byte; residual: a client that exits and whose PID is reused in the microseconds before the read could be mistaken for another allowed account), and creates the next instance before returning so there is no window with no listener. 5 integration tests on the real pipe (tests/broker_pipe.rs): the SID read back is this account's and bytes flow; two clients connected at once both get through; a second listener on the same name fails; a DACL admitting only SYSTEM refuses this account (known-bad control); bad names and SIDs, including an SDDL-injection attempt, are refused. **Not shown:** a client in a different process (the tests' client is this process) or account (needs a second account, which is CireSnave's), a client that disconnects mid-accept, handle-limit behaviour under many connects. Not built: the broker binary (config, main loop wiring `HyperV` + `serve_connection` + this). Version provisional (PM allocates; stacked on #92's 0.0.103).

## 0.0.103

- Broker slice V3, fourth part: `broker_serve::serve_connection`, one broker connection over any `Read`/`Write` (frames in with the existing length-prefixed `session_proto` framing, `Broker::handle`, frames out). An intact but invalid frame (extra field, unknown op) is answered with an error and the loop goes on; a bad length or truncated frame ends it; **on every exit the connection's VMs are destroyed** (`Broker::disconnect`). The caller passes the client's SID as `account` and the broker's clock. 4 tests over in-memory streams (a vanished peer's VM is destroyed; bad frame then good one; truncated frame still cleans up; a `u32::MAX` length ends it). **Not shown:** a real pipe, reading the client SID from the OS, concurrent connections. Note: `read_frame` allows frames up to 1 MiB, far above any broker request (about 700 bytes); a tighter limit is a later change. Still not built: the broker binary (pipe, config, main loop). Version provisional (PM allocates).

## 0.0.102

- Broker slice V3, third part: the cloud-init seed. `seed::files` produces the three NoCloud files (`user-data`, `meta-data`, `network-config`) as text: one account `antspawn` with the key, key-only SSH (`ssh_pwauth: false`, password locked, root login off), a static `<guest>/30` address on the NIC and **no gateway** (the guest has no route off its switch, so the file does not invent one), hostname and instance id the VM id. The key is re-validated and only its type and base64 blob are written, never the comment; the id must be ours and the address must be inside 10.77.0.0/16. `hyperv` now builds the seed in `create`: the files into a folder, a `cidata` ISO (IMAPI2FS, ISO 9660 plus Joliet, the usual IStream helper compiled by PowerShell), the folder removed, the ISO attached with `Add-VMDvdDrive`; `delete` removes the ISO and any leftover folder. `VmSpec.pubkey` is no longer ignored. 5 new tests (text of the three files; a hostile comment `evil:{x}'#` reaches no file; bad key, id or address refused; create script has the seed after `New-VM` and no comment; a bad key stops `create` before the runner is called). **UNVERIFIED and unrun:** that IMAPI2 builds the ISO from PowerShell as the broker account (a COM object; it may need an interactive session or the account's profile), that a real cloud-init reads these files from it, the `sudo: ALL=(ALL) NOPASSWD:ALL` choice (the design says the runner runs whatever it wants inside the VM; the VM is the boundary), the NIC match `e*`, and the guest's interface name. Also unmeasured: the first run of the whole create script. Still not built: the broker binary that wires `HyperV` to the pipe. Version provisional (PM allocates).

## 0.0.101

- `scripts/broker-pool.ps1`, broker slice V3's switch pool (`docs/SANDBOX-BROKER.md`). Elevated, run by CireSnave, never by a lane; dry run by default, `-Apply`, `-Undo`. Slot i (1..N, default 4): Internal switch `antspawn-sw<i>`, host address `10.77.<i>.1/30` (guest `.2`, inside what `account-firewall.ps1` already allows), IPv4 forwarding Disabled, no NAT, and an inbound Block rule `antspawn-pool-in-<i>` bound to the switch's interface. `-Apply` refuses (creating nothing) if any 10.77.x address already exists, any pool switch or rule exists, or `state.json` exists; writes `state.json` after EACH slot so `-Undo` always knows what exists; reads every slot back (switch type, address, prefix, Preferred, forwarding, rule direction/action/enabled and interface filter) and stops at the first mismatch; reads the state folder's ACL back (Administrators and SYSTEM only). `-Undo` removes only entries in `state.json` that carry our prefixes and reads their absence back. Compatible with Windows PowerShell 5.1 and 7. Shown without elevation: it parses; the dry run prints the 4 slots and **really refuses on this machine** because the V0 switch `antspawn-v0-sw` still holds 10.77.0.1 (so `broker-v0.ps1 -Undo` must come first); non-elevated `-Apply` is refused; `-Slots 99` is refused. The read-back function, run against the real V0 switch, passes every check it can (Internal, address, prefix 30, Preferred, forwarding Disabled) and reports only the missing rule; it reports a wrong host address and an absent switch. **Elevated-only and unrun:** `New-VMSwitch`, `New-NetIPAddress` on the new interface, `Set-NetIPInterface`, `New-NetFirewallRule -InterfaceAlias` on an interface seconds old, the icacls, `Remove-VMSwitch`, and `Get-NetFirewallInterfaceFilter -AssociatedNetFirewallRule` (never run against a real rule of ours). What the inbound Block does to a guest probing the host is unmeasured (V6). Version provisional (PM allocates).

## 0.0.100

- `session.toml` `launcher_sid`: the shape check (PM `[TASK] bind-launcher-sid-to-spool-owner`, a reviewer's held-back finding on code on main) required only the `S-1-` prefix and digits, `S` and `-`, so `S-1-`, `S-1--` and `S-1-5-` passed and failed later with a vague error. It now needs `S-1-<authority>-<sub-authority>...`: every part a non-empty run of digits, an authority and 1 to 15 sub-authorities. 11 bad values added to the existing shape test. Shown failing against 0.0.98's check: the test panics on `S-1-` (the first new value; the test stops at the first failure, so the others are not each shown, they follow from the old check accepting any string of digits, `S` and `-` after the prefix). **The binding the finding asks for is already on main** since 0.0.77 (S3): `spawn-runner session` reads the OS owner of the spool entry directory and of `session.toml` (`session_pipe::owner_sid`) and refuses, before any pipe exists, a `launcher_sid` that is not that owner (`SessionFile::check_owner`, unit test, and `tests/session_host.rs` `a_session_naming_someone_who_does_not_own_the_spool_entry_is_refused_before_any_pipe_exists`). Not covered by that check: an owner who has been given the right to write for someone else's SID (it needs SeRestore or an owner change the OS lets only an administrator make); an elevated launcher's files are owned by Administrators and are refused loudly. Version provisional (PM allocates).

## 0.0.99

- Broker slice V3, second part: the base-image hash check. `HyperVConfig.base_hashes` pins a SHA-256 per offered image; the create script runs `Get-FileHash` on the base and throws before `New-VHD` if it differs, and an image with no pinned hash, or a hash that is not 64 hex digits, is refused before any script runs. Through PowerShell, so no new dependency. 2 new tests (the hash check precedes the first disk; unpinned or malformed is refused and the runner is never called). **Cost unmeasured:** it hashes the whole base on every create (a multi-GB disk is seconds to tens of seconds); a cache keyed on size and mtime would be weaker and is not done. Still has not touched Hyper-V. Not built: seed ISO, pool switches, broker binary. Version provisional (PM allocates).

## 0.0.98

- Broker slice V3 (`docs/SANDBOX-BROKER.md`), first part: `hyperv`, the real Hyper-V `Driver`, built on the cmdlets `scripts/broker-v0.ps1` measured working for a non-elevated Hyper-V Administrator (`New-VHD -Differencing`, `New-VM` gen 2, `Set-VMMemory`/`Set-VMProcessor`, `Disable-VMIntegrationService`, `Stop-VM -TurnOff`, `Remove-VM`). Scripts go to `powershell.exe` on stdin (never a command line); only resolved values and one validated name go in, each as a single-quoted literal after `quote` refuses control characters and non-ASCII; a name without the `antspawn-` prefix is refused before anything runs, so `delete` can never touch `my-laptop-vm`; `delete` is idempotent by construction. The process is behind a `Runner` trait: 6 tests on every CI leg check the generated script text (resolved sizes, switch and base path in, the public key not in, a hostile image name stays inside its literal, a newline in a switch is refused, a foreign name never reaches the runner). **Nothing here has touched Hyper-V**: the script text is untested against the real cmdlets, the first real run is the runner's. **Not built yet:** the seed ISO (so the guest gets no key and no address, `VmSpec.pubkey` is ignored), the pool switches, and the base-image hash check; the broker binary that wires `HyperV` to the pipe. Version provisional (PM allocates).

## 0.0.97

- `spawn-wfp` and `antspawn::wfp`, Part A of the account network boundary (PM `[TASK] account-firewall-rule-script-and-write-up`; CireSnave 2026-11-08 "403: C"; separate from Part B so an unrun failure here cannot take it down). Windows Firewall rules cannot refuse a restricted account's connections to loopback or the host's own addresses (the existing all-block rule for `antspawn-run` is enabled and 32 local ports still answer it); Windows Filtering Platform filters can. `spawn-wfp apply` adds, in ONE transaction, a persistent provider and sublayer with fixed keys and two persistent **Block** filters at `ALE_AUTH_CONNECT` v4 and v6 whose conditions are `ALE_USER_ID` granted by `D:(A;;CC;;;<account SID>)` and the `IS_LOOPBACK` flag; it then reads each filter back by key and removes them again on any mismatch. `undo` removes exactly those four objects by their fixed keys and reads back that they are gone; `plan` and `status` change nothing. Built on `windows-sys` (typed structs and GUIDs, so layouts are compiler-checked), not hand-written P/Invoke. CI's `runner-exe` job now also builds and hashes `spawn-wfp.exe` into the same artifact (`spawn-wfp.exe` and `spawn-wfp.exe.sha256`).
  Shown without elevation: 6 unit tests (SID resolution and the SDDL-injection refusal; the descriptor round-trips, names the SID and grants no generic-all, and depends on the SID; the plan has two connect-layer Block filters with the user and loopback conditions and the filter's condition pointers resolve to the descriptor bytes; the plan survives a move; the four keys are distinct); `spawn-wfp plan` prints the exact objects; `spawn-wfp status` runs the real `FwpmEngineOpen0` and `FwpmFilterGetByKey0` as a normal user and reports both filters not present.
  CodeQL `rust/access-invalid-pointer` on the read-back (`src/wfp.rs`, the dereference of the out-pointer after `FwpmFilterGetByKey0`): the pointer starts null and was trusted on `rc == 0`; it is now read with `as_ref` (null becomes an error). Defence only: the API returns a filter on success.
  UNRUN and elevated-only: `apply` and `undo`. What a failure leaves behind: any failure inside `apply` aborts the transaction, so nothing; a failed commit, nothing; a read-back mismatch after commit removes our objects again (and says if that removal itself failed, in which case `undo` finishes it). `undo` is one transaction too. UNKNOWN until the per-port scan after `apply`: whether `IS_LOOPBACK` also covers a connect to the host's own non-127 addresses (172.25.224.1, 10.77.0.1). COST: a job that starts a server and connects to it on loopback breaks; nothing in this repository's use of the account does. Version provisional (PM allocates).

## 0.0.96

- `docs/ACCOUNT-ISOLATION.md` (PM `[TASK] measure-what-the-restricted-account-can-reach`), with the two connect-only scripts in `scripts/`: the `account` row `network.off` is true for external addresses (the OS refuses the socket: `1.1.1.1:53` and `93.184.216.34:80` as `antspawn-run` give "forbidden by its access permissions") and **not for this machine's own addresses**. Every TCP port on `127.0.0.1`, 1 to 65535: `antspawn-run` and the interactive user reach the same 32 ports (135, 445, 5037, 11434, 49664 to 49671 and others); `172.25.224.1:135/445` and the broker listener `10.77.0.1:2222` answer too. A port with no listener times out in both columns (the method's control). No byte was sent to any service, so what a reached service accepts is untested. Owning process names could not be obtained (the tool shell and the account job both show only 8 ephemeral sockets with process id 0); the page gives the one command the machine's owner can run. LAN address, IPv6, UDP, named pipes and the other runner accounts are not measured. No firewall change was made; a fix is elevated and CireSnave's. Docs and scripts only. Version provisional (PM allocates).
- `docs/ACCOUNT-ISOLATION.md`: read back the account's existing firewall rule (`antspawn-run: block all outbound`: Enabled, Outbound, Block, Any profile, protocol and remote address, bound to the account's SID) and drew the two consequences: a Windows Firewall rule by user SID cannot refuse loopback or the host's own addresses (this all-block rule exists and they still answer), and the same rule makes a runner's SSH to its own VM at a pool address impossible (Block beats Allow), which answers V0 unmeasured item 4 for the current rule as "no". Anything that must be isolated from the machine's services belongs in a VM or Windows Sandbox. The "what a fix would need" section is rewritten to two changes (Windows Firewall pool exception, WFP filters) instead of one; nothing written or applied.

## 0.0.94

- `scripts/account-firewall.ps1`, Part B of the account network boundary (PM `[TASK] account-firewall-rule-script-and-write-up`, CireSnave 2026-11-08 "403: C"). Elevated, run by CireSnave, never by a lane; dry-run by default, `-Apply`, `-Undo`. Replaces the blanket `antspawn-run: block all outbound` rule, which also stops a runner from reaching its own VM at a pool address (Block beats Allow), with three `antspawn-fw-*` rules for the account's SID: block everything outside `10.77.0.0/16` (two IPv4 ranges plus all of IPv6), block TCP to the pool except port 22, block UDP to the pool. The old rule is DISABLED, not deleted, and only after the new rules are created and **read back** (Get-NetFirewallRule plus the security, address and port filters, address ranges compared as ranges, refuse on mismatch); `state.json` records that this script disabled it, in a folder whose ACL is read back, and `-Undo` re-enables it only on that record and removes only `antspawn-fw-*` rules. It does NOT touch loopback or the host's own addresses (Windows Firewall rules do not see them; that is Part A, a separate script). Shown without elevation: the script parses and its dry run prints the three rules exactly; `New-NetFirewallRule -WhatIf` accepts the address and port forms and rejects a bad address and a bad port; the read-back code run against the real existing rule matches it and catches a wrong protocol, address, port and SID; the address canonicaliser treats `::/0` and the full v6 range, and `/16` and the explicit range, as equal and a different pool as unequal. Elevated-only and unrun: the creation of the IPv6 range, `Set-NetFirewallRule -Enabled False`, the icacls on the state folder, and the form the address filter returns after creation. A failure before the old rule is disabled leaves it in force. ICMP to the pool is not blocked (stated, not measured). Version provisional (PM allocates).


## 0.0.92

- Broker slice V2 (`docs/SANDBOX-BROKER.md`): `broker`, the VM broker's core behind a `Driver` trait, tested against an in-memory fake on every CI leg (the real Hyper-V driver is V3 and waits on a host). A request is validated (`broker_proto`) before the driver sees anything, and the driver is handed resolved values (a profile's sizes, a pre-created network slot), never request text but the one validated key. A VM is owned by its creating connection; a disconnect, a lifetime expiry or a failed create destroys it; a network slot is held until the driver confirms the VM is gone, so a failed delete is retried (`tick`) and its slot is never handed out meanwhile. At start the broker removes VMs carrying its `antspawn-` prefix that it is not tracking and nothing without the prefix, and refuses to start if it cannot. The VM broker answers `sandbox_start` with an error. 11 tests, among them: a refused request leaves the driver call log empty, a create that fails after half-making a VM leaves none and gives the slot and the count back, another connection (same account) can move and see nothing, the leftover sweep keeps `my-laptop-vm`. Nothing here is exercised against a real peer or Hyper-V. Version provisional (PM allocates).

## 0.0.91

- `scripts/broker-v0.ps1`: `New-LocalUser -Description` is limited to 48 characters and ours was 64, so CireSnave's first `-Apply` stopped there (nothing but the folder was created; `-Undo` cleans it). The description is now `AntSpawn V0 broker. Remove: broker-v0.ps1 -Undo` (47). A new check at the top of the script, which the dry run runs too, tests every string given to the account and task cmdlets against its limit (user name 20, description 48, generated password 67 of 127, task names, the task `-User` string) and refuses a user name with a character a local user cannot have; a temp copy with the old 64-character text is refused by it. Checked against the cmdlet itself with `New-LocalUser -WhatIf` not elevated: the new text binds, the old one gives the same "Cannot validate argument ... 48" error. Not exercised until elevated: `Set-BatchRight` (secedit export and import of the batch-logon right), `Register-ScheduledTask -User/-Password` for a user that needs that right, `SetSecurityDescriptor` on the tasks, and everything the probe then does as the broker. Version provisional (PM allocates).
- `scripts/broker-v0.ps1`, second finding from CireSnave's patched `-Apply` (user, group, batch right and both tasks were OK; the probe task returned 0x1 and wrote no `probe.txt`): the folder was granted `antspawn-v0-broker:(RX)` with no `(OI)(CI)`, so `probe.ps1` inherited only the launcher, Administrators and SYSTEM and the broker could not read its own script. The grant on the folder is now `(OI)(CI)RX` (`results` stays `(OI)(CI)M`), each `icacls` is checked, and after the files are written `-Apply` prints `icacls` for both and reads the ACL back, failing unless the broker has a read-and-execute allow ACE (a granted ACL is not a read ACL until read back). Shown in a throwaway folder with the Users group standing in for the broker: the old `(RX)` leaves 0 matching ACEs on a file created inside, the new grant leaves 1. Not shown: the real run as the broker, which only the elevated rerun does.
- `scripts/broker-v0.ps1`, third finding (first probe, measured by CireSnave, relayed by the PM): as `antspawn-v0-broker` (Hyper-V Administrators, not elevated) `Get-VM`, `New-VMSwitch` Internal (7.3 s), `New-VHD` and a differencing child all worked and the new switch had forwarding Disabled, but `New-VM` failed with `failed to set folder permission for results: Access is denied (0x80070005)`. The broker had Modify on results\, which lacks WRITE_DAC, and Hyper-V sets the VM's own SID on the disk folder as the caller. The grant is now full control (still that folder only) and `-Apply` reads that ACE back too. A throwaway folder shows the rule (Modify: 0 ACEs with ChangePermissions; full control: 1); that this fixes `New-VM` is NOT yet shown, only the rerun does. `docs/SANDBOX-BROKER.md` records the first probe as measured and the full-control-of-its-own-disk-directory need as a consequence still to be confirmed.
- `scripts/broker-v0.ps1`, fourth finding (second probe, relayed by the PM): `New-VM` failed with `Sequence contains more than one element` because the first run's `antspawn-v0-sw` was still there when the second created another one of the same name (Hyper-V allows that). The probe now refuses, creating nothing, when a switch named `antspawn-v0-sw` or a VM named `antspawn-v0-vm` already exists, and `-Apply` refuses before creating the user or tasks, both pointing at `-Undo`. The probe also records how many adapters and switches answer to the name before the host-address step, whose result changed between run 1 (access denied) and run 2 (OK) without a known reason. Shown: the probe's refusal path against an existing switch name (stand-in `Default Switch`): exit 3, the REFUSED line, no VM or switch created. Not shown: a clean single-switch rerun.
- `scripts/broker-v0.ps1`, fifth finding (first listener, relayed by the PM): the listener died one second after it started. `TcpListener.Start()` failed because the new host address was still Tentative ("The requested address is not valid in its context"), and then the loop called `Pending()` on the never-started listener, which ended the task (`LastTaskResult` 1). After a failed start the listener now sets `$tcp = $null` (the loop already guards on it) and the pipe side carries on, and `-Apply` waits up to 30 s for the address to be Preferred and fails loudly if it is not. Shown: the old listener text and the fixed one run against an unassigned address (and a test pipe name, so a real listener is never in the way): old ends in `FATAL Not listening`, fixed logs the failure and reaches `END`. Not shown: the real listener under the broker account.
- `docs/SANDBOX-BROKER.md`: the clean V0 run is recorded (the whole VM lifecycle including the power cut works as a non-elevated Hyper-V Administrator with full control of its own disk directory; the host address stays the elevated step's), with the unmeasured remainder stated.

## 0.0.90

- Sessions, slice S5 (`docs/SESSION-PLAN.md`): `spawn session [--launcher account]`. One process is one session (the host ends it if the process dies), driven by lines: a `session_proto` request per line on stdin (`exec`, `close`), a reply per line on stdout. A bad or refused line is answered with an `error` and the session stays usable; a line above the 1 MiB frame cap ends it; end of input closes it. A launcher the matrix does not certify is refused with the missing capability named (seen live before the rows changed: `launcher `account` is not certified for: session`). Matrix rows `session` and `clean-slate-next-session` for `account` are now `yes`, hand-written from two new probes in `probes::account` (state kept across two execs; nothing there in a second session after `close`), which run in `tests/account_session.rs` and are UNVERIFIED where the setup or the redeployed `spawn-runner` is missing. `no-read-other-session` stays `no`. Measured once, live, on the redeployed exe: whoami `antspawn-run`, state kept across execs, close answered; 5.8 s for the whole 5-line run. Not measured: end of input without `close` on the real account, concurrent sessions, a second user as launcher. Version provisional (PM allocates).


## 0.0.89

- `docs/MEASUREMENTS.md` 2026-11-08: `cargo test --test account_session` on the redeployed `spawn-runner.exe` (CI artifact of main `3333a683`, sha256 `3A5102B3...4E3`): 1 passed in 3.83 s, no `UNVERIFIED`; a session on `antspawn-run` keeps state between execs and leaves nothing after close. One run; the second-user launcher, latency and concurrency are not measured. Docs only. Version provisional (PM allocates).


## 0.0.88

- Broker slices V1 and SB1 (`docs/SANDBOX-BROKER.md`): `broker_proto`, the pure request vocabulary and validation shared by the VM broker and the Windows Sandbox broker. No pipe, no Hyper-V, no account: nothing here is exercised against a real peer. `Request` is `create` (image + profile from fixed sets + one `ssh-ed25519` key line), `stop`/`start`/`delete`, `list`, `sandbox_start`; unknown ops and fields are errors. No field can hold a host path, every name must be plain (no separator, dot-dot or drive letter), a key may not carry `authorized_keys` options or a second line, and refusals name the field, never the value. `Ledger` records who owns what: only the creating connection may act on a VM, caps (total, per account) and a per-account create rate come from the SID the broker read, a disconnect releases that connection's VMs, and a lifetime ceiling expires the rest. A known-bad request list is run against the real validator (all refused) and against naive validators (caught). Caps in `Limits::default()` are a chosen number, not a measurement. Version provisional (PM allocates).

## 0.0.87

- `docs/SANDBOX-BROKER.md` revision 3 (PM `[TASK] windows-guest-profile`; CireSnave: "Windows Hyper-V can't create a Windows guest?"): corrects revision 2, which said the VM broker does not replace Windows Sandbox for Windows jobs. Adds a Windows guest profile with image source and licence, disk, boot time, runner access (OpenSSH vs PowerShell Direct), updates and activation, and what Sandbox does better, each stated as measured or unmeasured (only host disk space was measured; no Windows image, boot time or licence was checked), plus slice V6. Adds slice V7 and a Windows Sandbox broker track SB0-SB3 (broker in CireSnave's session, same validation; hardening under a limited account as the last slice of each track), and `scripts/broker-v0.ps1` (V0's elevated measurement, dry-run by default, `-Undo` rolls back). `scripts/sb0-broker.ps1` and the SB0 result (MEASUREMENTS 2026-11-07): a pipe works between `cires` and `antspawn-run` (control refused), and a Sandbox starts from the broker's session on that account's request, ready in 12 to 20 s (4 runs), stopped when the client leaves. Keeps Windows Sandbox and `account` until V6 measures. Docs only. Version provisional (PM allocates).


## 0.0.86

- Row 9 re-run (MEASUREMENTS 2026-11-07): `windows_sandbox` 5/5 green, the stale SAC control bites again (one run, interactive, KB5127936 present); ROADMAP row 26 step 1 done. Docs only.
- CI: job `runner-exe` builds `spawn-runner.exe` (release, `--locked`) on GitHub-hosted `windows-latest`, prints its sha256 and uploads it with a `.sha256` file as artifact `spawn-runner-windows-x64` (PM ruling 2026-11-06: the redeploy installs the CI-built artifact, never a lane- or box-built exe, and the artifact hash is compared with the installed file). `actions/upload-artifact` pinned to v7.0.2 by SHA. UNMEASURED until it runs on GitHub: that the artifact builds and downloads, and that its bytes match across two builds of one commit (no reproducible-build claim is made). Version provisional (PM allocates).

## 0.0.85

- Sessions, slice S4 (`docs/SESSION-PLAN.md`): close and abandon. (1) A session's commands get `TEMP`/`TMP` = `work/<sid>.tmp` (`Layout::job_scratch`), and `serve` wipes it with the work dir BEFORE publishing `exit.code`; what a command writes through `TEMP` no longer lands in the account's profile temp. A command that sets its own `TEMP` or writes to the profile directly is not covered: the profile is not wiped (UNMEASURED what else the account's profile accumulates). (2) A watcher: `run_command` takes an `alive` check polled every 20 ms and the host passes `PeekNamedPipe` on the session pipe, so an owner that dies DURING a long exec gets the command killed then (`Exit::Killed`, `session.end` = `disconnected`), not after the exec; `tests/session_host.rs` drops the pipe 1.5 s into a 60 s `ping` and the real host exits (control: the same exec with a live owner is bounded only by its limit). (3) Orphan reaping: `runner::reap_ended_sessions` removes the spool and results entries of sessions that published `exit.code` at least 120 s ago and were never collected (owner killed before `close`); `Account::open_session` calls it first. Jobs (`spec.toml`), running sessions and recent ones are left alone (unit-tested with a control that the same call reaps them once old enough). UNMEASURED: all of this as the real account (needs the redeploy), a session whose HOST died with no `exit.code` and no runner pass (it is picked up by the next `serve`, which waits 60 s for a client that never comes). `a_command_past_its_limit_*` made deterministic (ROADMAP row 27): it failed 1 of 3 on the S3 baseline and in two full runs here under load; the command now runs ~29 s unless cut and the bound is 20 s, so the check no longer sits next to the full run time. Version provisional (PM allocates); stacked on #74.
- Review fixes (pip, #75): `close()` on a session the reaper already collected returns Ok instead of waiting 60 s for an `exit.code` that cannot reappear and then erroring (test with a control that the ordinary close still cleans up); a scratch dir that cannot be created now stops the command (exit 126, reason in stderr) instead of running it with a dead `TEMP`. The silent-wipe finding is fixed through #74's `fresh_dir`, which now also covers the scratch dir. The survivor control in `a_disconnect_ends_the_host_and_a_survivor_dies_with_its_job` polls up to 15 s for the heartbeat to grow instead of a fixed 2.5 s sleep (it failed 2 of 5 runs under load before; 6 of 6 after).

## 0.0.84

- Sessions, slice S3 (`docs/SESSION-PLAN.md`): `spawn-runner session <id>` hosts one session and `Account::open_session` opens it. The launcher writes `spool/<id>/session.toml` (its own SID from `whoami /user`, a 128-bit nonce), triggers the `AntSpawnRunner` task, and connects to `\.\pipe\antspawn-session-<id>-<nonce>`; `serve` starts the host for a session spool entry as it starts `exec`, wipes the work dir after the host exits and only then publishes `exit.code`. The host joins a kill-on-close Job Object, creates a one-instance duplex pipe with a protected DACL naming the launcher SID only (`0x12019f`, remote clients rejected), serves that one client until `close` or a disconnect, then terminates the job. The platform-independent half is `session_host` (`serve_session` over any `Read + Write`, `run_command`, `SessionFile` with SDDL/path-safe validation), unit-tested on every leg: intact-but-invalid frames are answered and the session goes on, a bad length or truncated frame ends it, nothing invalid reaches the executor. `tests/session_host.rs` runs the real host as the current user in a temp layout (no account): the named SID connects and state persists across execs, another SID is refused (control: the right SID connects), a second client finds the pipe busy, and a disconnect ends the host and kills a detached survivor (control: it was writing while the session was up). `tests/account_session.rs` is the runner-only test as the real account and prints UNVERIFIED until the deployed `spawn-runner.exe` includes `session`. The `session` matrix row stays `no` (S5). UNMEASURED: a session as the real account (needs a `spawn-runner` redeploy), a remote client being refused, a killed owner during a long exec (the host notices the disconnect only between frames; S4 adds a watcher), per-exec descendants outliving their exec (they live until the session ends, by design). Version provisional (PM allocates); stacked on #72.
- Review fixes (pip, #74): **the host now verifies `launcher_sid` against the OS-reported OWNER of the spool entry (directory and `session.toml`) and refuses a session naming anyone else**, before any pipe exists (the DACL rested on a launcher-writable file; `docs/SESSION-PLAN.md` says so). An elevated launcher's files are owned by `Administrators` and are refused loudly. Tests: a SID that is not the owner is refused with no pipe created; the owner SID is accepted; the pipe DACL itself (moved to the library module `session_pipe`, in-process) refuses a SID it does not name. Also: a session ended by a broken frame publishes `killed`, not `0`; a work dir that cannot be wiped refuses the session (also for jobs) instead of silently leaking the previous contents; a timed-out accept leaks its pipe handle on purpose (documented) instead of closing it under the blocked wait thread; `pipe_name` refuses an id with a separator. UNMEASURED: a second real user as the launcher (needs a second account). The survivor control in `a_disconnect_ends_the_host_and_a_survivor_dies_with_its_job` polls up to 15 s for the heartbeat to grow instead of a fixed 2.5 s sleep (it failed 2 of 5 runs under load).
- CodeQL (cleartext-logging, #74): the `pipe_name` backstop panic no longer prints the session id. The id grants nothing (the pipe is guarded by its DACL on the launcher SID, not by its name), but a rejected id is untrusted text and need not be echoed.

## 0.0.83

- Sessions, slice S2 (`docs/SESSION-PLAN.md`): `session_proto`, the pure wire format between a launcher and a session host. A frame is a 4-byte little-endian length then JSON; the length is checked before anything is allocated or read (0 and above 1 MiB refused; a test fails if the reader is asked for a byte beyond the header). `Request` is `exec` or `close`, `Response` is `exec`, `closed` or `error`; unknown ops and unknown fields are errors, so the later `cancel` and `output` requests are refused by an older host, not ignored. The host re-runs `ExecRequest::validate` on what it receives. No pipe, no process, no account: nothing here is exercised against a real peer (S3). Stacked on #71. Version provisional (PM allocates).
- Review fix (pip): `Response::Exec` now carries `combined` (omitted when false, so older frames still read as not combined); it was dropped to `false` on the wire. A test round-trips a combined outcome and reads a frame without the field.

## 0.0.82

- `docs/MEASUREMENTS.md` 2026-11-06 (board 321 task): `wsb list` and `wsb start` run AS `antspawn-run` (non-interactive, package registered) both return `Cancelled`; the same call works interactively. Candidate cause, untested: `antspawn-run` is not in `Hyper-V Administrators` (`gh-runner` is). Cheapest discriminating experiment and the cost of the group fix are in the entry. Docs only. Version provisional (PM allocates).

## 0.0.80

- `docs/SANDBOX-BROKER.md` (PM `[TASK] sandbox-broker-options`, revised by `[TASK] vm-broker-revision`; CireSnave 2026-11-07, both quotes in the note): a **general VM broker**. Any runner in an allowed group asks over a named pipe for a VM from a fixed image set, SSHes into it, may stop/start/delete it, and every VM is owned by the pipe connection that created it (runner exit or disconnect destroys it; hard lifetime ceiling; orphan cleanup by `antspawn-` name prefix). One dedicated account holds Hyper-V Administrators so every runner account can leave the group. Controls table (no host paths in requests, a pre-created Internal switch pool with no NAT and forwarding off, integration services off, no shared folders/passthrough/GPU/nested virtualization, read-only base plus differencing disk deleted at kill, caps and rate limits) with a status for each; the Linux power-cut test (`kv.durability.machine`) as five broker verbs; whether it replaces Windows Sandbox (Linux yes, Windows toolchain jobs no); the Windows Sandbox broker (P0 to P2) kept short; slices V0 to V5. Measured today (read-only, `MEASUREMENTS.md`): Hyper-V state, switches, forwarding Disabled, only WSL registered with the Hyper-V firewall. NOT measured: a pipe between two accounts, a non-interactive account creating VMs, the per-VM firewall for a non-WSL VM, guest reachability from a restricted account. Docs only. Version provisional (PM allocates).

## 0.0.74

- Sessions, slice S1 (`docs/SESSION-PLAN.md`): the `Session` trait (`exec`, `close`), `ExecRequest` with validation, `Launcher::open_session` (default: "does not provide sessions") and `Registry::open_session`, which re-checks the matrix for the needs plus `session` and refuses with the unmet capability named, never falling back. Three capabilities, all off: `session`, `clean-slate-next-session`, `no-read-other-session`. `naive` gets a deliberately leaky session (one fixed directory, `close` wipes nothing) as the known-bad control: `Probes::session_keeps_state` (positive control) and `violates_clean_slate_session` run against it and the matrix records `clean-slate-next-session` = `no` for `naive` (MEASURED here; `matrix --check` fails when that row is mutated to `yes`). No launcher that isolates has a session yet: `account` and `windows-sandbox` rows are `no` with no probe, so `Registry::open_session` refuses them. UNMEASURED: everything about `account` sessions (S2-S5). Version provisional (PM allocates).

## 0.0.73

- `docs/SESSION-PLAN.md`: slice plan for the PM's task A1/A2/A3 (agent-loop Tier S): session on `account` over a named pipe (S1-S5), pool of N accounts (P1-P4), both A2 workspace modes per CireSnave's board 373 ruling (`bind` and `copy`, one setting; W0-W5, with W0 measuring ACL ordering first). Docs only; nothing is built, every cost and unmeasured item is listed. Also W0 measured (`docs/MEASUREMENTS.md` 2026-11-06, one sample): an explicit grant on a subtree beats the inherited `C:\Projects` deny, an explicit exact-rights deny on a subpath beats the grant, and icacls `W` also denies reads, so exact rights are needed. Version provisional (PM allocates).

## 0.0.72

- `docs/SESSION-OPTIONS.md`: options note sizing the agent-loop asks A1 (long-lived job), A2 (in-place workspace) and A3 (N concurrent sessions) per launcher: slices, what each costs in isolation, what is unmeasured, and the A2 questions for CireSnave (listed, not decided). Docs only; nothing is built. Version provisional (PM allocates); stacked on #66.
## 0.0.71

- Board 371, C2 option A (PM task): `docs/proof/gpu-probe.ps1` + generated `gpu-probe.yml` (s3: is the GPU usable from a restricted runner account) and `docs/proof/run-proof.ps1` (s1: one command after C1). MEASURED here as the interactive user: the probe passes (RTX 4070 Laptop, driver 617.42), the known-bad control (tools hidden) is refused, the two extracted yml steps give the same results, and `run-proof.ps1` aborts with a FAIL row when the `spawn` binary is missing. UNMEASURED: the probe as `ghr-*` (that is its purpose), the control only hides the tool by flag (it shows the probe can fail, not that a GPU-less account would fail), and `run-proof.ps1` end to end (needs C1, the runner group and the pushed workflows). `--fresh` stays the default; s4 (a persistent cache dir) waits on CireSnave. Version provisional (PM allocates).
## 0.0.69

- `account-setup.ps1`: two read-back checks that FAILed on the 2026-11-02 elevated re-run (log `C:\Users\Public\wsb-register-20261102-211456.log`) while the account was fine. Cause found in the Task Scheduler log (`Microsoft-Windows-TaskScheduler/Operational`) and the live task: **(1) "task actually started ... exited 0"**: the task started and completed at 21:23:50 with result 0, but `LastRunTime` has whole-second resolution and the start time `t0` had a fraction, so `LastRunTime -ge t0` was false for a task started in that same second (the wait also ran its full 60 s); `t0` is now truncated to the second. **(2) "task allows <launcher> to run it"**: matched the text `GRGX`, which Windows prints as `0x1200a9` once a task is re-registered (the false FAIL `runner-account-setup.ps1` fixed in 0.0.61); it now uses `Get-TaskSdProblems`. Both exposed by 0.0.67 because a re-run now re-registers the task. MEASURED here: the live `AntSpawnRunner` descriptor gives 0 problems with the new check and the text match is False; a wrong launcher SID gives a problem (control); the truncation compare is False before and True after for a run and start in the same second. UNMEASURED: a second elevated run of the script. Also seen in that run: `wsb list` is Cancelled for `antspawn-run` too, so all three runner accounts behave alike (still unexplained). Version provisional (PM allocates).

## 0.0.67

- Windows Sandbox package (`wsb.exe`) is now part of runner-account creation. New `scripts/AntSpawnSandboxPkg.ps1` (dot-sourced) registers `MicrosoftWindows.WindowsSandbox` AS the account (`Start-Process -Credential -LoadUserProfile` running `Add-AppxPackage -RegisterByFamilyName`), the approach the PM measured on a throwaway account on 2026-11-02 (S4U task: Access denied; provisioning: no `.main` file). `runner-account-setup.ps1` and `account-setup.ps1` call it right after the random password is set, before the task is registered, and read back `InstallState` (Get-AppxPackage -AllUsers) and `wsb.exe` under the account's profile as PASS/FAIL. **Behaviour change:** both scripts now reset the password on every `-Apply` (the package step needs it) and re-register the task with `-Force`, so a re-run on an existing account replaces its task. `account-setup.ps1` also drops `RandomNumberGenerator.Fill` (not in Windows PowerShell 5.1) for the loop `runner-account-setup.ps1` already uses. `docs/proof/PASTE-sandbox-register.ps1` is the one elevated paste for `antspawn-run`, `ghr-spawn1`, `ghr-spawn2`, with PASS/FAIL per step, an independent read-back table and an UNDO block. `wsb list` as the account is printed, not gated: it returned `Cancelled` on the throwaway account (UNEXPLAINED); `wsb list` exits 0 for the interactive user, who is in `Hyper-V Administrators`, an untested hypothesis that a runner account must not act on. MEASURED here: all three files parse; both dry-runs print the new step; the child script's output format, with a bogus package name (negative control: `REGISTER=FAIL`) and `wsb list` exit 0 as the interactive user. UNMEASURED: the registration as ghr-spawn1/ghr-spawn2/antspawn-run (elevated, not run by a lane), the re-registered tasks, the PASTE file end to end. Version provisional (PM allocates).

## 0.0.66

- ROADMAP row 13 (toolchain loading cost per job): `examples/toolchain.rs` now reports median **and min to max**, and `PAD_MIB=<n>` adds an unread filler file to scale the toolchain. Re-measured on the (loaded) host, 5 rounds, 54.2 and 566.2 MiB: `none`, `mapped` and `copied` are not distinguishable (the bare VM alone ranges 9.2 to 23.4 s), so the 0.0.42 figure "1.1 to 1.4 s per job" is **retracted** in `docs/MEASUREMENTS.md`, not just caveated. A golden image does not exist in Windows Sandbox and for `account` needs elevated setup, so it stays unmeasured. Nothing in the launchers changes.

## 0.0.65

- `spawn adapter` retries a failed dispatch (ROADMAP row 24, the #59 reviewer finding). **Before:** a minted job was remembered at once and never re-minted, and the real dispatcher runs the job on a background thread, so its failure (task not started, config not delivered) was never reported at all: `take_outcomes` had no caller. **Now:** `Dispatch::finished()` reports how each dispatched job ended (`Outcome` gains `nothing_ran`), the watcher reads it at the start of every tick (`TickReport::{ended_ok, ended_failed}`), and a job whose runner provably never started (`schtasks /Run` failed, or the config was not delivered and the backend ended the task) is minted for again after 120 s, 3 attempts in all (`RetryPolicy`, compiled in, not in the config schema). An error after the config was delivered is final. `--once` settles after the wait, never retries, and exits 3 on a runner that ended in an error. Tests: 5 new in `watcher`, 1 in `dispatch`, 1 extended in `command`; three mutations (drop the `nothing_ran` condition, drop the attempt cap, drop the cooldown) each fail 1 to 2 of them. Measured on the real account `ghr-spawn1` (`examples/dispatch_failure.rs`, `docs/MEASUREMENTS.md`): a missing task and an undeliverable config both leave the task Ready, no pipe, no shim. `.coderipper.toml`: the `take_outcomes` `public_api` entry is removed (it now has a caller; coderipper reports a waiver that matches nothing), leaving 8 reachability waivers. UNMEASURED: a failure after delivery, the retry against GitHub, the Linux CI result. One run of `cargo test` here failed 3 of the 5 `--test account` tests (`Account::provision`/`run` `expect` at `src/probes.rs:162-163`); the next run passed all 5, as did main `d446332`; the cause was not found (ROADMAP row 25: the panic messages were lost to my output filter; not reproduced in 5 sequential runs and 2 concurrent ones).

## 0.0.64

- `.coderipper.toml` (from 0.0.63) quoted the test input's header and footer lines literally, so SEC-002 matched the config itself in commits `7e21e131` and `104cdadb`. Reworded so no current line matches, and a second `[[allow]]` names those two commits with its reason. Measured (coderipper 0.8.19, same flags, this tree): SEC-002 0 findings, 4 waived; reachability 0 findings, 9 waived; before the reword SEC-002 had 1 unwaived finding on `.coderipper.toml:6`. `docs/MEASUREMENTS.md` also records the 2026-10-31 spawn1/spawn2 pipe probes and the unexplained `LastTaskResult` 2.

- docs/proof/: the files for the account-backed 3-run proof (ROADMAP row 21 slice d): `adapter.toml` (two account groups), `proof.yml` (positive, clean-slate and negative controls A and B, written so a step passes only on a proven denial), `PASTE-spawn1.ps1`, `PASTE-spawn2.ps1` (second runner account and canary; elevated), `make-dpapi-key.ps1` (PEM to the DPAPI key file; dry-run by default; tested with a dummy PEM only) and `README.md` with the order. None run end to end. Version provisional (PM allocates).

## 0.0.63

- `.coderipper.toml`: the repository's CodeRipper waivers, each with its reason and owner, none blanket. **SEC-002** (gitleaks `private-key`) flagged line 591 of `src/adapter/github_client.rs` in two history commits, `e0130079` (the `pr13-github-client` branch only) and `d5f1c62d` (on main). Read at both: the line is the 96-character test input `-----BEGIN RSA PRIVATE KEY-----\n!!!SECRETBYTES!!!\n-----END RSA PRIVATE KEY-----`, fed to `app_jwt` to show a malformed key is refused and never echoed; it is not a key, and no current tree holds a literal header (the fixture is built at run time by `fake_pem`). **Reachability** (9 rows, `coderipper check reachability`): `take_outcomes`, `WsbBackend`, `clamp_time`, `ListenerTransport`, `VERSION`, `verify`, `verify_against`, `collect_logs`, `job_script_in` are library API with callers only in tests until the sandbox path is wired (boards 321/322). Measured with coderipper 0.8.19, `--profile extended --deny medium --no-config`, on main `aaf814d`: without the file SEC-002 gave 2 findings and reachability 9 (exit 1); with it both give 0 findings, 2 and 9 waived (exit 0).

## 0.0.62

- Review fixes on #59: `spawn adapter --once` exits **3** when a dispatch failed or a runner was still running when the wait ended (it exited 0 before); exit codes are documented (`docs/ADAPTER-GITHUB.md`); each dispatch logs its job, group and launcher; `RoutingDispatch::unrouted` (dead) removed; `--check` of an `account` group off Windows says to run it on the Windows host; a failed dispatch is documented as not retried (ROADMAP row 24). **Behaviour change:** `--once` exit code on failure 0 -> 3.

- `spawn adapter --config <file> [--check | --once] [--pem-key]` (ROADMAP row 23 slice c, which gives the adapter its first caller and so clears the 11 true `reachability` findings of row 14 once the CI sweep agrees): loads the configuration through `adapter::config_file::load`, builds `HttpGithub` (DPAPI key file by default, plain PEM with `--pem-key`), a `Minter`, a `Watcher` and a `RoutingDispatch`, and polls every 20 s (`--once`: one poll, then wait for the runner it started). `--check` is offline: it validates the file, that each group's launcher can take a config, and that `key_path` is an absolute existing file (the key is not read). Only the `account` launcher is routable (Windows, the pipe path); a group naming `windows-sandbox` makes `--check` fail and the adapter refuse to start, naming the group and board 322, rather than mint a runner it cannot deliver. Account groups declare their own `task` and `pipe` (new optional group keys in the schema and `RunnerGroup`; required and unique for `account`, refused for any other launcher), served by `PerGroupAccountBackend`. Runner ends are logged to stderr (group and result, never the config). **Not run against GitHub or a real account**: `--check` was run on this machine, `run` and `--once` were not (UNMEASURED). Version provisional (PM allocates).

## 0.0.61

- `scripts/runner-account-setup.ps1`: the check "task allows the launcher to run it and nothing beyond read+run" gave a **false FAIL** on CireSnave's first real `-Apply` (2026-10-31; 11 of 12 checks passed): it matched the text `GRGX`, but Windows stores the mask as `0x1200a9`, and Task Scheduler adds the run-as account (`FR`) and inherited BA/SY entries. It now decodes the descriptor numerically (`scripts/AntSpawnTaskSd.ps1`): the launcher must hold exactly `0x1200a9`, the run-as account read only, BA/SY anything, any other principal or any non-Allow entry fails. `tests/task_sd_check.rs` uses the descriptor read back from that machine (SIDs from it) with a control that the old text match fails it, and cases for an extra principal, write and change-permission bits, a missing launcher, a run-as write, and a deny entry. The check was not run elevated against the live task by me; the string is the one `GetSecurityDescriptor(4)` returned to a standard user.

## 0.0.59

- `scripts/runner-account-setup.ps1` **failed on its first real run** (CireSnave, 2026-10-31, 0.0.58): `New-LocalUser -Description` accepts at most 48 characters and ours had 59, so `-Apply` stopped at its first action (read back: no user, no folder, no task; only the shim installed). It had only ever been dry-run. Fixes: (1) the description is a 42-character constant, independent of `-Name`; (2) the whole body is in `try`/`catch` that prints `FAILED: ...` and exits 1, because an uncaught `throw` left `$LASTEXITCODE` at 0 when the script was called with `&` (the "exit non-zero on any FAIL" promise held only for the read-back checks); (3) the random password is regenerated until it has an upper-case letter, a lower-case letter and a digit (local password policy), and uses `RandomNumberGenerator.Create().GetBytes` (works in Windows PowerShell 5.1; `Fill` needs .NET Core). Tests `tests/runner_account_setup_script.rs`: every `-Description` is at most 48 characters with a 16-character `-Name` (and a control that the old 59-character text is flagged), and a forced throw gives exit code 1 through `&`; both fail on the 0.0.58 script. Parameters audited against documentation, not by running elevated: see the PR. **Still unverified: the script has not completed an `-Apply` anywhere.**

## 0.0.58

- `adapter::routing::RoutingDispatch` (ROADMAP row 23 slice b; `docs/ADAPTER-DECISIONS.md` decision 3, ruled A): one `Dispatch` per launcher id, chosen by the group's `launcher`. One job at a time across all launchers (`busy()` is true while any is busy; a busy flag per launcher waits on a measurement). An unknown launcher is refused naming group and launcher, never the JIT config; `unrouted()` lets `--check` list groups with no dispatcher. Nothing calls it yet: the `spawn adapter` command is slice c. Version provisional (PM allocates).

## 0.0.57

- `adapter::config_file::load` and the schema `configurant.toml` (ROADMAP row 23 slice a; `docs/ADAPTER-DECISIONS.md` decision 1, ruled B): the adapter's configuration loads through ConfigurAnt, giving a row-indexed error with a did-you-mean for an unknown key (`group[1].launcer`, suggestion `launcher`) and `configurant check/explain` on the schema. The configuration file keeps the shape `AdapterConfig::from_toml` accepts, and a test loads the sample through both and requires equal results; the rules a schema cannot say (unique group names and label sets, exact `owner/name` repos) stay in `AdapterConfig::validate`. New dependency `configurant` 0.5.33 (crates.io; `cargo deny` clean, one duplicate `windows-sys` warning). **Breaking (pre-1.0 notice): the minimum Rust version is now 1.95** (ConfigurAnt needs it; CireSnave 2026-10-08 permits raising it; the CI msrv job reads it from `Cargo.toml`). Nothing calls `load` yet: the `spawn adapter` command is slice c. Version provisional (PM allocates).

## 0.0.56

- docs: `docs/ADAPTER-DECISIONS.md`, the three decisions `spawn adapter` (ROADMAP row 23, which clears the 11 true `reachability` findings of row 14) needs before it is built: where the configuration lives (plain TOML vs a ConfigurAnt schema), where the GitHub App key comes from (DPAPI/file now, vault env later, SecretAnt does not exist yet), and sandbox vs account per group. Options, evidence and a recommendation each; nothing run. Rulings (PM, 2026-10-31) recorded; both unverified points checked (published configurant 0.5.33 loads `[[table]]` rows; a multi-line value survives an environment variable between processes). Version provisional (PM allocates).

## 0.0.55

- CodeRipper baseline (ROADMAP rows 15-17), no behaviour change: the five discarded `watcher::tick` results in tests now assert what they return (`unused-return-values`); the `allow(dead_code)` in `examples/matrix.rs` gets a reason (`lint-suppression`); the test fixtures in `adapter::github_client` no longer contain a full PEM header literal, they build it at run time (`SEC-002`, gitleaks `private-key`). The SEC-002 cause is a **hypothesis** (the rule's pattern spans lines, and the three fixtures sit close together; the reported line 591 is not a key) that gitleaks could not be run here to confirm: CI is the check. Row 14 (`reachability`) is unchanged and recorded as a finding that is true: no binary drives the adapter yet. Version provisional (PM allocates).

## 0.0.54

- `spawn-jit-shim --fresh` (ROADMAP row 21 slice f): the runner's `TEMP`, `TMP`, `USERPROFILE`, `APPDATA` and `LOCALAPPDATA` now point into `<copy>\_home`, so its scratch space is deleted with the copy. This **reduces** what a job leaves for the next one; a job that writes the account's real profile or temp folder by absolute path still can, and nothing here proves otherwise. Two tests (the variables point inside the copy and the copy is gone; a control shows the same script outside the shim sees the real folders); not run as a restricted account (UNMEASURED). Version provisional (PM allocates).

## 0.0.53

- `spawn-jit-shim --fresh <template> <runs> <runner>` (Windows only; ROADMAP row 21 slice e, design item 4: wipe between runs). After the config arrives the template is copied to a new `<runs>\run-<pid>-<nanos>`, the runner (a path relative to the copy, no `..`) runs there, the shim joins a kill-on-close Job Object so the runner's whole tree is in it, ends every other process in the job when the runner exits, and deletes the copy; a copy that cannot be deleted is exit 3. A symlink in the template is refused; a config that is not one creates no copy. Seven tests as the current user with two controls (a shared directory does carry state; a plain delete does fail while a process holds a file); **not run as a restricted account or via Task Scheduler** (UNMEASURED). `scripts/runner-account-setup.ps1` now keeps the runner as a read-only template (`<root>\runner-src`) and runs the task with `--fresh`; still never applied. The account's temp and profile folders are not wiped yet. Version provisional (PM allocates).

## 0.0.52

- `scripts/runner-account-setup.ps1` (ROADMAP row 21 slice c): one-time elevated setup for one account-backed runner (`ghr-<Name>`, private root `C:\gh-runner-<Name>`, pinned-runner copy, `SeBatchLogonRight`, a task running `spawn-jit-shim --pipe` as the account that the launcher may run). Dry-run by default, `-Apply` reads the state back as PASS/FAIL. **Never applied anywhere**; only the dry-run output and the argument guards were run. It does not set a firewall limit (not expressible as Windows Firewall rules), does not wipe the runner directory between runs, and does not start the task. Version provisional (PM allocates).

## 0.0.51

- `adapter::account_backend` (ROADMAP row 21 slice b): `AccountBackend<T: AccountTask>` (a `RunnerBackend`: start the account's task, hand the pipe name to delivery, wait; a failed delivery ends the task), `PipeTransport` (a `JitTransport`: opens `\\.\pipe\<name>` as the launcher user, retries until the shim has created it, writes once; busy and denied are errors at once) and `SchtasksTask` (the real task, `schtasks /Run`, `/Query`, `/End`). Tested with a fake task and, on Windows, against the real `spawn-jit-shim --pipe` as the current user, with a denied-principal control. **Not run against a real account, task or Task Scheduler**; `schtasks /Query` output format and whether a non-admin `/Run` works are UNMEASURED. Not yet wired into `spawn adapter`. Version provisional (PM allocates).

## 0.0.50

- `spawn-jit-shim --pipe <name> --allow-sid <sid> <runner>` (Windows only; ROADMAP row 21 slice a, board 341): receives the JIT config on a named pipe with one instance, remote clients rejected, `FILE_FLAG_FIRST_PIPE_INSTANCE` (a squatter's name is refused), and a protected DACL with a single ACE for `--allow-sid` (write only). Same limits, checks and in-memory-only handling as stdin and `--listen`. Six tests against the current user, with a denied-principal control; **not run against a second account or via Task Scheduler** (needs the elevated provisioning, UNMEASURED). Found by measurement: SDDL `FW` alone denies the named SID; the ACE needs `FILE_READ_ATTRIBUTES` too. Adds windows-sys features `Win32_Security_Authorization`, `Win32_Storage_FileSystem`, `Win32_System_IO`, `Win32_System_Pipes`. Version provisional (PM allocates).

## 0.0.49

- **Breaking for callers of `spawn run`:** a refusal exits **125** (was 2) and another spawn-side failure **126** (was 2); job exit codes, 124 (timeout) and 137 (killed) are unchanged. New `<results>/spawn-result.json` (authoritative status), `stdout.tail` and `stderr.tail` (last 64 KiB; `windows-sandbox` combined). Output names `stdout.tail`, `stderr.tail`, `spawn-result.json` are reserved. `naive` now gives its job a null stdin and captures output instead of inheriting it. `account` streams need a redeployed `spawn-runner` (UNVERIFIED). MEASUREMENTS: the "sandbox never starts from a tool shell" statements are marked superseded (0.0.29). Version provisional (PM allocates).

## 0.0.48

- docs: `docs/ACCOUNT-DISPATCH.md`, the design note for an account-backed Dispatch (ROADMAP row 21); docs only, nothing run. Records board 341 ruled A (CireSnave 2026-10-30): named ACL'd pipe and outbound network allowed on this path. Version provisional (PM allocates).

## 0.0.47

- docs: `docs/PATENT-SWEEP.md`, the patent sweep of antspawn 0.0.47 (8 ideas, 11 dated Google Patents queries, 13 patents' independent claims read, all `clear`, three watches). Espacenet returned nothing and USPTO/WIPO were not used; the record says so. ROADMAP row 20 updated. Version provisional (PM allocates).

## 0.0.46

- measure: `examples/toolchain.rs` and the toolchain-loading-cost numbers (MEASUREMENTS): mapped vs copied vs bare VM.

## 0.0.45

- docs: ROADMAP.md, the work source (HANDOFF is state, not a queue); one row per failing CodeRipper rule.

## 0.0.44

- ci: CodeRipper portfolio workflow pin moved from 30e012b7 (v0.9.x era) to 74996d75, the commit v0.15.0 points to (`git rev-parse v0.15.0^{commit}` on a clone; the tag object is 0cac390d). Findings may change; listed in the PR. Version provisional (PM allocates).

## 0.0.43

- `adapter::jit_host::ListenerTransport`: the real `JitTransport` (`wsb ip`, then one connection to the shim's listener). Piece 3 of board 322; **not run against a VM until board 321 is settled**. Version provisional (PM allocates).

## 0.0.42

- `adapter::jit_host`: the host end of the board 322 transport: `guest_ip` (`wsb ip --id`, accepts only one unambiguous IPv4), `send_once` (connect with retry until the guest listener is up, write the config once, close; errors never contain it), `deliver_to_sandbox`. Loopback-tested with controls; **not run against a VM until board 321 is settled**; the `wsb ip` output format is unmeasured. Version number provisional (PM allocates); branches off main, independent of #43.

## 0.0.41

- `spawn-jit-shim --listen <addr:port>`: guest-side one-shot listener for the JIT config (board 322 ruled A). Accepts exactly one connection, closes the listener, refuses a second or later one, exits after one. Stdin mode unchanged. Tested on loopback with controls; **not run in a VM until board 321 is settled**. Version number provisional (PM allocates).

## 0.0.40

- docs: HANDOFF rewritten to current state.

## 0.0.39

- docs: the 0.0.32 / 0.0.30 statements that the Windows Sandbox guest blocks unsigned executables (and that `job.cmd`'s Smart App Control switch-off is needed) are
  **withdrawn until re-proven**: on 2026-10-29 the raw-guest control of `unsigned_exes_start_repeatedly_*` fails on unmodified main (guest in
  evaluation, 0x2, every start allowed). Evidence and the open mechanism question: MEASUREMENTS "Stale control". The isolation matrix rows do not rest on it.

## 0.0.38

- `adapter::dispatch`: the real `Dispatch` (`SandboxDispatch`), one runner VM per minted JIT config on a background thread, `busy()` until it ends, outcomes without the secret. Two seams: `RunnerBackend` (real one `WsbBackend`, built on the new `windows_sandbox::RunnerVm`) and `JitTransport`. **No real transport yet**: it waits on board 322. Unit-tested with fakes; not run in a VM.
- `windows-sandbox`: `WsbEnv::run` split into `run_hooked` (calls a hook with the sandbox id once the VM is up); behaviour for ordinary jobs unchanged.

## 0.0.37

- `examples/measure.rs` runs the `windows-sandbox` launcher (mapped folder + VC++ runtime) and prints each concurrent job's error. Measured: run 5.5 to 6.9 s, destroy 3 s, one VM at a time (`CO_E_APPSINGLEUSE`); see MEASUREMENTS.

## 0.0.36

- `windows-sandbox`: `runner_wsb_config` (network on, only `in`, `out` and the pinned runner mapped) and `runner_job_script` (SAC off in the guest, copy the runner, start line, `exit.code` last). Unit-tested, not run in a VM; waits on board 321/322 (the JIT-config transport is not built).

## 0.0.35

- docs: measured that `wsb exec` forwards no stdin (board 260 hand-off needs another channel into the guest).

## 0.0.34

- docs: HANDOFF rewritten to current state.

## 0.0.33

- Dependency: `capant` 0.0.16 -> 0.1.0 (breaking upstream: result types, `Role` and `Outcome` are `#[non_exhaustive]`). `cargo check --all-targets` needed no change here; `matrix --check` still up to date.

## 0.0.32

- `windows-sandbox`: `job.cmd` switches Smart App Control off INSIDE the guest first (SYSTEM, repeated until `0x0`), so the runner's unsigned
  binaries and repeated unsigned starts work (board 260, PM ruling A). Host policy is never touched: the commands exist only in the guest
  script, a unit test counts their uses, and the new test compares the host's state before and after 3 consecutive VMs, with a raw-`wsb`
  control that is blocked. `docs/DESIGN.md` states the hypervisor is the isolation boundary.
  The new test is runner-only, not run by CI (it needs Windows Sandbox); it was run locally (MEASUREMENTS). A self-hosted runner for it awaits board item 321.

## 0.0.31

- Dependency: `capant` 0.0.7 -> 0.0.16 (newest on crates.io). No API change needed; `matrix --check` still up to date.

## 0.0.30

- `windows-sandbox` matrix probes (`probes::sandbox`), run as `cmd` scripts of signed tools: the guest runs Smart App Control, which blocks an
  unsigned exe after its first start (measured). Each probe checks its own tool first (exit 3 = inconclusive); the same script on the host is the
  known-bad control. `spawn-probe` answers "no" with exit 10 (was 1) so a blocked run cannot read as isolated; `account` follows.

## 0.0.29

- `windows-sandbox` now drives the package's `wsb.exe` (`start`, `exec -r System`, `stop --id`) instead of
  `WindowsSandbox.exe <file>.wsb`, whose identity-less client crashed on `WinRT.Runtime` (board 253, measured). The proof
  through it passes from a tool shell; no `LogonCommand`, no `taskkill` by image name.
- `run` fails within about 2 s, naming the Application-log event, when a `WindowsSandbox*.exe` crash follows the request.
- A run killed outright leaves its VM; the next run stops it (`antspawn-wsb-<id>.owner` in the temp dir, own ids only).
- New `tests/windows_sandbox.rs`: guest cannot read `C:\Projects` or the host profile, a read-only mount is not writable,
  network off, orphan reaped; the same probe on the host is the known-bad control. UNVERIFIED where the sandbox is absent.
- CI: the advisory `portfolio` job repinned to coderipper 30e012b7 (PM notice). `docs/MEASUREMENTS.md`, `docs/DESIGN.md`.

## 0.0.28

- Docs: `HANDOFF.md` rewritten to current state. No code change.

## 0.0.27

- Board 260 (CireSnave, "A: 1 B: 1"), host end of the runner hand-off: `spawn-jit-shim <runner> [args...]` reads the JIT config
  from stdin (an anonymous pipe), refuses anything that is not plausible base64 (empty, over 64 KiB, not text), and starts the
  runner with `ACTIONS_RUNNER_INPUT_JITCONFIG` set on that one child only; nothing prints the value. `adapter::handoff::start_shim`
  starts the shim and writes the `Secret` to its stdin. Tests (`tests/jit_shim.rs`) include controls: a wrong value and an unset
  variable fail the same check. NOT built: running the shim inside Windows Sandbox (the only place the runner gets network),
  which waits on the sandbox starting; the watcher still has no real `Dispatch`.

## 0.0.26

- First real regeneration of `matrix/launchers.tsv` with CapAnt 0.0.7: the file gains a `probed_at` column (`check` ignores
  it). No `environment` column (ruling A, 2026-10-24: one committed file must match CI on every OS). `matrix --check` still
  drops `inherited_from` from the generated side, and now prints the carried-over rows it hides (generated side) and any
  committed rows marked inherited (`Report::carried`), so review sees which rows rest on trust.
- `account` probes re-run on this host (5/5 `yes` rows re-earned), recorded in `docs/MEASUREMENTS.md`.

## 0.0.25

- Docs: `CLAUDE.md` records the ruled JIT-config exception to "secrets are names only" (CireSnave, board 260, "A: 1 B: 1"):
  in-memory pipe to a shim for the runner's JIT config, runner network only inside Windows Sandbox. No code change.

## 0.0.24

- The matrix is CapAnt's: `matrix.rs` wraps `capant::parse` (still refusing an unknown capability or a duplicate row), `Registry`
  ranks and re-checks through `capant::evaluate`, and `examples/matrix.rs` is a CapAnt `Prober` run by `capant::generate` and
  `capant::check`. `matrix/launchers.tsv` is unchanged and `matrix --check` is green. New rule from CapAnt: a `yes` row must name its
  probe, or it grants nothing. `--check` ignores the `inherited_from` column that CapAnt adds to carried-over rows, until the first
  regeneration commit.
- Docs: `docs/MEASUREMENTS.md` records Windows Sandbox host health (package intact; the `WinRT.Runtime` crash is a tool-shell
  launch-context failure, cause unknown).

## 0.0.23

- Fix: `windows-sandbox` reported `TimedOut` for any job that exited with a one-digit code (so every success). `job.cmd` wrote
  `echo %RC%> exit.code`; cmd reads `0>` as a handle redirect, so `exit.code` stayed empty and `run` waited out `time_s`. The
  script now writes `>file echo %RC%`. Host tests run the generated script (`job_script_in`) and include the old form as a
  known-bad control.
- `windows-sandbox`: the guest writes `trace.log` (start, command exit) and `cmd.log` (the command's stdout and stderr);
  `collect` returns them as `windows-sandbox.trace.log` / `.cmd.log`. `run` fails at once if `WindowsSandbox.exe` itself
  exits non-zero before the job finished. `slangc_proof` prints both logs and, on failure, Hyper-V/Sandbox/WER events.
- `github_proof perms <adapter.toml>`: prints the installation's permission names and levels (from the token exchange's
  `permissions`; the token itself is dropped unread). `HttpGithub::installation_permissions`.
- `LICENSE-MIT` holder is now "The antspawn contributors" (CireSnave, decision 263; the company holder waits on ownership).

## 0.0.22

- `github_proof queued <adapter.toml> <owner/repo>`: runs the PR19 `JobSource` live, read-only. First live run is recorded in
  `docs/MEASUREMENTS.md`: refused, the App lacks the repository `Actions: read` permission. `JobSource` was UNVERIFIED until 0.0.23 (see there).

## 0.0.21

- CI: the advisory `portfolio` job is repinned to coderipper `789b012` (coderipper#125). Advisory, not required.

## 0.0.20

- CI: the advisory CodeRipper portfolio check (`portfolio` job, reusable workflow pinned to `c665210`). Not a required check; no rule is silenced.

## 0.0.19

- `HttpGithub` implements `watcher::JobSource`: `queued(repo)` lists the repo's `queued` and `in_progress` runs (a later job of a
  multi-job workflow waits inside a running run), reads each run's jobs (`filter=latest`) and keeps those with status `queued`,
  with their `runs-on` labels. Pages of 100, at most 5 pages per listing. A run that vanishes between the calls (404) is skipped;
  any other failure fails the poll, so the watcher never forgets a job it merely could not see. **verified against GitHub 2026-10-23 (0.0.23: one queued job listed, see MEASUREMENTS)**
  until run live against `antspawn-proof`; the parsing is unit-tested on fixed JSON only.

## 0.0.18

- `antspawn::adapter::watcher`: the watcher loop as pure logic. `Watcher::tick(now)` polls the configured repos through a
  `JobSource`, routes each queued job, mints one just-in-time runner through the `Minter` and hands it to a `Dispatch`.
  One job at a time (nothing polled while busy, at most one mint per tick, oldest first), a job is minted for once, a failed
  mint stays queued under the minter's backoff, unrouted jobs are left alone, a failing repo does not stop the others. The
  JIT config reaches only the dispatcher; `TickReport` holds no secret. Fake source, API and sink in the tests; no real
  `JobSource` or `Dispatch` exists yet (the dispatch waits on board 260), so nothing here touches GitHub.

## 0.0.17

- `account` is certified: `matrix/launchers.tsv` now has `yes` rows for `isolation.no-read-outside`,
  `isolation.no-write-outside`, `clean-slate-next-run`, `kills-runaway-process` and `network.off`, each from a probe that ran
  through the real restricted account on 2026-10-23 (`docs/MEASUREMENTS.md`). `gpu.*` and `concurrent` stay `no`.
  Before this, `spawn run --launcher account` refused every spec.
- New probes: `connect <host:port>` in `spawn-probe`; `Probes::violates_network` (needs a control: the host itself must reach
  the address, else no verdict) and `probes::account::*`, the account launcher's probes, shared by `tests/account.rs` (now five
  tests, adding write-outside and network) and `examples/matrix.rs`. `naive` must reach the network (`tests/negative.rs`).
- `examples/matrix.rs`: `account` is probed where its setup is applied; elsewhere (CI) its committed rows are carried over,
  only if complete and every `yes` names its probe. `windows-sandbox` rows are carried over too (nothing probes it yet).
  `--no-privileged` forces the carry-over path.

## 0.0.16

- `antspawn::adapter::runner_cache`: `ensure_archive` (cached copy reused only if it passes the pin, download renamed
  into place only after it passes, a bad cached file is deleted), `verify_file` (streamed), `unpack` (refuses a
  path-climbing entry and removes the partial result; refuses an existing destination). `examples/runner_fetch.rs` is the
  live proof. New dependency: `zip` (deflate only). The runner is still not started.

## 0.0.15

- `antspawn::adapter::runner_pin`: the pinned `actions/runner` release (v2.338.1 win-x64: URL, size, SHA-256) and
  `verify`, which refuses bytes whose size or digest differ. `docs/RUNNER-PROVENANCE.md` records origin, the three-way
  digest check and licence; the MIT text is kept in `third_party/actions-runner/LICENSE`. Nothing fetches, unpacks or
  starts the runner yet.

## 0.0.14

- `tests/slangc_proof.rs`: the tools directory now carries `msvcp140.dll`, `vcruntime140.dll` and `vcruntime140_1.dll`
  (copied at test time from the host's installed Visual C++ Redistributable, never committed), which `slangc` imports and a
  clean Windows Sandbox lacks (exit 0xC0000135 STATUS_DLL_NOT_FOUND on the desktop run). New red-first test
  `staged_tools_carry_the_visual_cpp_runtime_slangc_imports`. The `MEASURE` line now splits provision/run/collect/destroy and
  prints before the asserts, so a failing run still reports its timings. No launcher code changed.

## 0.0.13

- `antspawn::adapter::github_client`: the real GitHub client (`HttpGithub`, `DpapiKeyProvider`, `app_jwt`, `classify`).
  Signs the App JWT with `ring`, exchanges it for an installation token, lists runner groups/repos/runners, mints a
  JIT runner config, deletes a runner. New `KeyError::Malformed`; `GithubApi` is implemented for `&T`.
  `examples/github_proof.rs` (Windows) is the live proof. New dependencies: `ureq`, `ring`, `base64`, `serde_json`
  (and `windows-sys` Win32_Security_Cryptography). `deny.toml` allows CDLA-Permissive-2.0 (webpki-roots).
  No watcher loop and no runner hand-off yet.

## 0.0.12

- `windows-sandbox`: `destroy` (and `Drop`) retry removing the job's temp root, up to 60 s (10 s on drop). From an
  interactive desktop the VM still held its mapped folders after being closed and removal failed with os error 32.
  `docs/MEASUREMENTS.md`: that run's results.

## 0.0.11

- `examples/measure.rs`: a job that times out or exits non-zero is now an error, not a timing. Before, a launcher whose
  jobs never completed (`windows-sandbox`, 2026-10-22) printed its own time limit as `run`.

## 0.0.10

- `tests/slangc_proof.rs`: the VM leg uses absolute guest paths (`C:\work\...`) and `cwd`, since a Windows Sandbox
  mapped folder cannot be relative; time limit 240 s. `docs/MEASUREMENTS.md`: the `account` slangc proof passes; every
  `windows-sandbox` attempt on this box timed out (see there). No launcher code changed.

## 0.0.9

- `scripts/account-setup.ps1`: no ACL deny on BinDir (its W right includes SYNCHRONIZE, which CreateProcess needs; the task
  failed to launch with 0x80070005) and removes one left by an older run; grants and checks `SeBatchLogonRight` itself.
  Measured live 2026-10-22: after both, the task runs as `antspawn-run`, LastTaskResult 0.

## 0.0.8

- `antspawn::adapter::github` and `docs/ADAPTER-GITHUB.md`: design and types for the GitHub App adapter (per-job
  `generate-jitconfig`, ephemeral runners). Redacted `Secret`, `KeyProvider` (a key path inside the job sandbox is
  refused), strict `AdapterConfig` with runner groups, group verification, `Minter` with backoff and no retry storm.
  No network or signing code yet; measurements stay UNVERIFIED.

## 0.0.7

- `examples/measure.rs`: times any launcher (provision, run, collect, destroy medians over N rounds; N jobs at once
  against a 2 s sleep for concurrency). `account` and `windows-sandbox` print UNVERIFIED while not set up.

## 0.0.6

- `tests/slangc_proof.rs`: PR4-proof harness. Runs `slangc` (Unpopped's shader compiler) through `account` and
  `windows-sandbox` and requires SPIR-V byte-identical to a direct host run; prints `MEASURE` timings.
  Runner-only: prints UNVERIFIED while a launcher is not set up. Not yet run against a real launcher.

## 0.0.5

- `examples/matrix.rs`: regenerates `matrix/launchers.tsv` by running the negative probes (`--check` for CI).
  `naive` rows come from real probe runs (all `no`, probe named); `account` and `windows-sandbox` are
  `no` / `UNVERIFIED: not set up` and are never `yes` without a privileged run.
- Probes moved to `antspawn::probes` (shared by the tests and the generator); new write-outside probe.

## 0.0.4

- `account` launcher + `spawn-runner` service binary: the job runs as a restricted account whose password
  nobody holds; spool/results directories, copy-in/copy-out, Job Object kill-on-close (the only `unsafe`,
  Win32 calls in `spawn-runner`), runner-side ceilings in `runner.toml`, loud `NotProvisioned`.
- `scripts/account-setup.ps1`: one-time elevated setup, dry-run default, `-Apply`, reads state back.
  Dry run tested; `-Apply` NOT yet run (needs CireSnave, elevated).
- `tests/runner.rs` (runner mechanics as the current user, passing) and `tests/account.rs` (real account;
  prints UNVERIFIED until the setup exists). `docs/MEASUREMENTS.md` started.
- New dependency: `windows-sys` (Windows only).

## 0.0.3

- `windows-sandbox` launcher: generated `.wsb` (no network, no vGPU, no clipboard; declared mounts only),
  `job.cmd` run at logon, `exit.code` polled, VM closed and host folders removed on destroy and on drop.
  cmd.exe metacharacters in the command, env or paths are refused, never escaped. Absent feature gives a loud
  `NotProvisioned`; the launcher never enables it. UNVERIFIED: no matrix row, no privileged run yet
  (the Sandbox feature is not enabled on the dev box).
- Fix: the two CLI tests shared one spec file and raced.

## 0.0.2

- `naive` launcher: a plain child process, the known-bad control. Never certified; `spawn run`
  refuses it for any spec that needs isolation.
- `spawn run <spec> [--launcher id] [--results dir]`, `spawn providers <spec>`, `spawn matrix`.
- Negative tests (read outside, clean slate, forked survivor) with the `spawn-probe` helper;
  `naive` must fail all three. Empty `matrix/launchers.tsv` (no launcher certified yet).
- Not in this PR: the `account` launcher (blocked on how it authenticates; see HANDOFF).

## 0.0.1

- Repository scaffold: CI (jobs build-test, msrv, deny, spdx),
  `deny.toml`, `CODEOWNERS`, `SECURITY.md`.
- Job spec types (strict TOML; `secrets` refused), capability set, matrix parser (PersistAnt's TSV
  format), launcher trait and provider selection (`providers`, `open_named`).
- `spawn check <spec>`.
