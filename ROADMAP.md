# antspawn ROADMAP

The work source. `HANDOFF.md` is current state for the next session and is rewritten at every restart; it is not a queue.
State is `open`, `held(by what)` or `done(PR)`. Add rows here, not to HANDOFF.

| # | Row | State |
|---|-----|-------|
| 1 | Scaffold, JobSpec types, capability set, matrix parser, launcher selection | done(#1) |
| 2 | `naive` launcher, `spawn run/providers/matrix`, negative controls | done(#2) |
| 3 | `account` launcher, certified (`cargo test --test account -- --test-threads=1` 5/5) | done(#4, #9, #17) |
| 4 | `windows-sandbox` launcher driving `wsb.exe`, 5 matrix `yes` | done(#3, #30, #32) |
| 5 | Capability matrix generator and CI diff | done(#5) |
| 6 | Measurement harness | done(#7) |
| 7 | GitHub App client, pinned runner fetch, `JobSource` over REST, watcher logic | done(#13, #15, #16, #18, #19) |
| 8 | Board 260 host end: `spawn-jit-shim`, `adapter::handoff`, runner VM config, `SandboxDispatch` | done(#8, #36, #38) |
| 9 | Stale SAC control: re-run `cargo test --test windows_sandbox -- --test-threads=1` after the NEXT Windows update; record date and result in MEASUREMENTS "Stale control". The guest-blocks-unsigned-exes claim stays withdrawn until the control bites again. Never touch host policy | held(next Windows update) |
| 10 | 260 runner slice: shim inside the sandbox (guest listener `spawn-jit-shim --listen` and the real `JitTransport`, `ListenerTransport`, are done: #43, #44, #45), a real run against `antspawn-proof`, a FULL runner job in 3 consecutive VMs, then tell the commy lane (R14) | held(board 322) |
| 11 | Self-hosted runner for `windows_sandbox` / `account` tests (rule 5.1g/h); until then named "runner-only, not run by CI" | held(board 321) |
| 12 | `schtasks /Run` by a non-admin vs a named pipe | held(elevation, CireSnave) |
| 13 | Measure toolchain loading cost per job: mapped vs copied vs bare VM measured (#42); a golden image is still unmeasured | open |
| 14 | CodeRipper `reachability`: 11 findings, `adapter::{dispatch,github,runner_pin,watcher}` and `launchers::windows_sandbox` items with no caller in this crate | open |
| 15 | CodeRipper `unused-return-values`: `watcher::tick` return value discarded at 5 of 18 call sites | open |
| 16 | CodeRipper `lint-suppression`: `allow(dead_code)` without a reason at `examples/matrix.rs:66` | open |
| 17 | CodeRipper `SEC-002` (gitleaks `private-key`, High) at `src/adapter/github_client.rs:591` in the CI sweep. Not reproduced locally: line 591 of the blob is a doc comment, and the `BEGIN RSA PRIVATE KEY` literals are test fixtures (lines 726, 727, 810; payload `QUJD`, i.e. `ABC`). Find what gitleaks matched, then fix or record a disposition. Never silence the rule | open |
| 18 | GPU in Windows Sandbox / Hyper-V GPU-P: unmeasured | open |
| 19 | Sandboxie-Plus with named pipes: unmeasured | open |
| 20 | CodeRipper `patent-sweep-record` (Medium/High): any version change fails it until `docs/PATENT-SWEEP.md` exists (first seen with the v0.15.0 pin, #46). Take the required content from CodeRipper's rule docs, not guessed | open |
| 21 | Account-backed Dispatch: a runner job on a per-runner restricted account for host-resource tests (cpufish AOCL). Design note `docs/ACCOUNT-DISPATCH.md` (the stdin pipe cannot cross accounts; named pipe; 3-run proof with a cross-account negative control; what needs elevation and what needs board 321). Slices: shim `--pipe`, `PipeTransport`/`AccountBackend` with fakes, setup script, live proof. Board 341 RULED A (CireSnave 2026-10-30, "341: A."): the named ACL'd pipe and outbound network are allowed, departing from 260(b). Live steps blocked on elevated provisioning, not on 321 | done(note); slices open |

Sweep source for 14 to 17: `coderipper sweep --profile portfolio` on `f08a257` (CI run 41015493600, coderipper 0.9.6); a local run
(0.8.19) reproduces rows 14 to 16 exactly and cannot run gitleaks.
