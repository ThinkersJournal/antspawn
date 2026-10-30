// SPDX-License-Identifier: MIT OR Apache-2.0
//! Adapters that feed AntSpawn jobs from an outside queue. Design notes sit beside the code in
//! `docs/`: `github` holds the types and policy (no network code), `github_client` the real client.

pub mod dispatch;
pub mod github;
pub mod github_client;
pub mod handoff;
pub mod runner_cache;
pub mod runner_pin;
pub mod watcher;
