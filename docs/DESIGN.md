# AntSpawn design (slice 1)

1. **Job spec** (TOML, strict): `[job]` name/cmd/cwd/env/secrets, `[[mount]]` host/guest/mode (`ro`, `rw`,
   `scratch`), `[limits]`, `[needs]` network/gpu/tools/require/prefer, `[[output]]` guest->name.
   `secrets` are **names only**; this version implements none and refuses a spec that lists one.
2. **Needs** = `require` (default: the isolation set) + `network.off` when `network = "off"` + the GPU
   capability when `gpu` is set.
3. **Launcher** trait: `id()`, `provision(spec) -> Environment`; **Environment**: `run`, `collect`,
   `destroy` (also on `Drop`). Launchers never elevate and never create accounts.
4. **Registry**: `providers(needs)` lists every launcher, satisfying first, in `prefer` order, naming each
   unmet need; `open_named(id, needs)` re-checks and refuses with the reason.
5. **Matrix**: PersistAnt's nine-column TSV in `matrix/`; a capability is on only for a `capability` row
   with `enabled = yes`. Parsing is strict.
6. **Tests**: types, parser and the `naive` launcher run on every CI leg. Tests that need a privileged
   account, an optional Windows feature or a runner (`account`, `windows-sandbox`) are runner-only and
   are **UNVERIFIED** in CI until a runner exists; they are never `#[ignore]`d. Local results are
   recorded with date and ref in `docs/MEASUREMENTS.md`.
