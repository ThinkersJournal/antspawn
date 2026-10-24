// SPDX-License-Identifier: MIT OR Apache-2.0
//! The launchers shipped with AntSpawn.

pub mod account;
pub mod naive;
pub mod windows_sandbox;

pub use account::Account;
pub use naive::Naive;
pub use windows_sandbox::WindowsSandbox;

use crate::launcher::Registry;

/// The recorded matrix, compiled in: what is certified is what was committed.
pub const MATRIX: &str = include_str!("../../matrix/launchers.tsv");

/// Every shipped launcher, certified by [`MATRIX`].
pub fn registry() -> Registry {
    Registry::new(
        vec![
            Box::new(Naive),
            Box::new(Account::default()),
            Box::new(WindowsSandbox),
        ],
        crate::matrix::parse(MATRIX).expect("committed matrix parses"),
    )
}
