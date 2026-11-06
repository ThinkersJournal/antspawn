// SPDX-License-Identifier: MIT OR Apache-2.0
//! AntSpawn: run one job in a clean sandboxed environment, collect its declared outputs, destroy
//! the environment. See `README.md` and `docs/DESIGN.md`.

pub mod adapter;
pub mod capability;
pub mod launcher;
pub mod launchers;
pub mod matrix;
pub mod probes;
pub mod result;
pub mod runner;
pub mod session;
pub mod session_host;
pub mod session_proto;
pub mod spec;
pub mod streams;

pub use capability::Capability;
pub use launcher::{
    Environment, Exit, LaunchError, Launcher, OpenError, OpenSessionError, Provider, Registry,
};
pub use matrix::Matrix;
pub use session::{ExecOutcome, ExecRequest, Session};
pub use spec::{JobSpec, Needs, SpecError};
