// SPDX-License-Identifier: MIT OR Apache-2.0
//! The negative controls (see `antspawn::probes`) against `naive`, which must FAIL every one.
//! Launchers that need privileged setup run them in runner-only tests (UNVERIFIED in CI).

use std::path::PathBuf;

use antspawn::launchers::Naive;
use antspawn::probes::Probes;

const PROBE: &str = env!("CARGO_BIN_EXE_spawn-probe");

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("antspawn-test-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn naive_fails_every_negative_control() {
    let dir = scratch("naive");
    let sentinel = dir.join("outside.txt");
    std::fs::write(&sentinel, b"secret").unwrap();
    let _ = std::fs::remove_dir_all(Naive::work_dir());
    let p = Probes::new(PROBE);

    assert!(
        p.violates_read(&Naive, &sentinel),
        "naive must read outside"
    );
    assert!(
        p.violates_clean_slate(&Naive),
        "naive must leave state behind"
    );
    assert!(
        p.violates_kill_runaway(&Naive, &dir.join("pid")),
        "naive must leave the forked process alive"
    );

    assert!(
        p.violates_write(&Naive, &dir.join("written.txt")),
        "naive must write outside"
    );

    let _ = std::fs::remove_dir_all(Naive::work_dir());
    let _ = std::fs::remove_dir_all(dir);
}
