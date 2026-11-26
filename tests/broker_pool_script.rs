// SPDX-License-Identifier: MIT OR Apache-2.0
//! `scripts/broker-pool.ps1` is CireSnave's elevated paste. Board 503 -Apply failed because
//! `C:\ProgramData\antspawn-pool` already existed with an explicit ACE for a user: `icacls /inheritance:r
//! /grant:r` replaces only the SIDs it names, so that ACE survived. These tests pin that the script rebuilds
//! the DACL from scratch (explicit entries reset) BEFORE removing inheritance and granting, and still reads
//! the result back. They do not show an elevated run works.

fn script() -> String {
    std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/broker-pool.ps1"),
    )
    .unwrap()
}

/// Index of the single code line (comments excluded) of `src` containing `needle`.
fn line_of(src: &str, needle: &str) -> Result<usize, String> {
    let hits: Vec<usize> = src
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with('#') && l.contains(needle))
        .map(|(i, _)| i)
        .collect();
    if hits.len() == 1 {
        Ok(hits[0])
    } else {
        Err(format!(
            "expected one code line with {needle:?}, got {hits:?}"
        ))
    }
}

fn problem(src: &str) -> Result<(), String> {
    let reset = line_of(src, "$Dir /reset")?;
    let inherit = line_of(src, "/inheritance:r")?;
    let readback = line_of(src, "state folder ACL is not Administrators+SYSTEM only")?;
    if !(reset < inherit && inherit < readback) {
        return Err(format!(
            "order must be reset ({reset}) < inheritance:r ({inherit}) < read-back ({readback})"
        ));
    }
    if !src.lines().nth(reset).unwrap().contains("$Dir") {
        return Err("/reset must target $Dir".into());
    }
    Ok(())
}

#[test]
fn the_state_folder_acl_is_rebuilt_from_scratch_not_patched() {
    problem(&script()).unwrap();
}

/// Known-bad control: the pre-fix script (no reset) must be refused by the same check.
#[test]
fn control_the_patching_script_is_refused() {
    let old = script().replace("$Dir /reset", "$Dir /noreset");
    assert_ne!(old, script());
    assert!(problem(&old).is_err());
}
