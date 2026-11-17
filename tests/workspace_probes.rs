// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `workspace.copy-back` and `workspace.bind` probes (slice W5 matrix rows), proven able to fail:
//! `naive` runs as the invoking user in an unisolated directory, so a session command can write the
//! real project folder by its absolute path (copy-back) and read an ungranted sibling (bind). Both
//! probes must say VIOLATED for it. The pass side is the real account, measured by
//! `cargo run --example matrix` (runner-only) and recorded in `docs/MEASUREMENTS.md`.

use antspawn::bind::Icacls;
use antspawn::launchers::Naive;
use antspawn::probes::Probes;

fn probes() -> Probes {
    Probes::new(env!("CARGO_BIN_EXE_spawn-probe"))
}

#[test]
fn naive_fails_workspace_copy_back() {
    assert!(probes().violates_workspace_copy_back(&Naive));
}

#[test]
fn naive_fails_workspace_bind() {
    assert!(probes().violates_workspace_bind(&Naive, Box::new(Icacls)));
}
