# Changelog

## 0.0.1

- Repository scaffold: CI (jobs build-test, msrv, deny, spdx),
  `deny.toml`, `CODEOWNERS`, `SECURITY.md`.
- Job spec types (strict TOML; `secrets` refused), capability set, matrix parser (PersistAnt's TSV
  format), launcher trait and provider selection (`providers`, `open_named`).
- `spawn check <spec>`.
