// SPDX-License-Identifier: MIT OR Apache-2.0
//! AntSpawn: run one job in a clean sandboxed environment, collect its declared outputs, destroy
//! the environment. See `README.md` and `docs/DESIGN.md`.

pub mod capability;
pub mod launcher;
pub mod matrix;
pub mod spec;

pub use capability::Capability;
pub use launcher::{Environment, Exit, LaunchError, Launcher, OpenError, Provider, Registry};
pub use matrix::Matrix;
pub use spec::{JobSpec, Needs, SpecError};
