// SPDX-License-Identifier: MIT OR Apache-2.0
//! AntSpawn: run one job in a clean sandboxed environment, collect its declared outputs, destroy
//! the environment. See `README.md` and `docs/DESIGN.md`.

pub mod adapter;
pub mod broker;
pub mod broker_client;
pub mod broker_config;
pub mod broker_pipe;
pub mod broker_proto;
pub mod broker_serve;
pub mod capability;
pub mod hyperv;
pub mod launcher;
pub mod launchers;
pub mod matrix;
pub mod probes;
pub mod result;
pub mod runner;
pub mod sandbox_broker;
pub mod sandbox_config;
pub mod seed;
pub mod session;
pub mod session_cli;
pub mod session_host;
pub mod session_pipe;
pub mod session_proto;
pub mod spec;
pub mod streams;
#[cfg(windows)]
pub mod wfp;
pub mod workspace;
pub mod wsb_driver;

pub use capability::Capability;
pub use launcher::{
    Environment, Exit, LaunchError, Launcher, OpenError, OpenSessionError, Provider, Registry,
};
pub use matrix::Matrix;
pub use session::{ExecOutcome, ExecRequest, Session};
pub use spec::{JobSpec, Needs, SpecError};
