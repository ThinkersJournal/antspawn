// SPDX-License-Identifier: MIT OR Apache-2.0
//! `docs/proof/PASTE-pool-accounts.ps1` stopped at its first account on its first real `-Apply` (2026-11-12):
//! `New-LocalUser -Description` takes at most 48 characters and ours had 49, and the dry run never calls
//! `New-LocalUser`, so it could not tell. These tests pin the fix as text (the script is applied only by
//! CireSnave, elevated): the description is a variable within the limit, and the limit check runs before
//! the first change, in the dry run too.

use std::path::PathBuf;

const LIMIT: usize = 48;

fn script() -> String {
    std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/proof/PASTE-pool-accounts.ps1"),
    )
    .unwrap()
}

/// Code lines only: the `<# #>` help block and `#` comments removed.
fn code(src: &str) -> Vec<String> {
    let mut in_help = false;
    let mut out = vec![];
    for l in src.lines() {
        let t = l.trim();
        if t.starts_with("<#") {
            in_help = true;
        }
        if !in_help && !t.is_empty() && !t.starts_with('#') {
            out.push(t.to_string());
        }
        if t.ends_with("#>") {
            in_help = false;
        }
    }
    out
}

/// Why `src` would fail a real run for its description, or `Ok(length)`.
fn description_problem(src: &str) -> Result<usize, String> {
    let lines = code(src);
    let uses: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("-Description "))
        .collect();
    if uses.len() != 1 || !uses[0].contains("-Description $PoolDescription") {
        return Err(format!(
            "-Description must be given the $PoolDescription variable exactly once: {uses:?}"
        ));
    }
    let defs: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("$PoolDescription = '"))
        .collect();
    if defs.len() != 1 {
        return Err(format!("expected one $PoolDescription line: {defs:?}"));
    }
    let l = defs[0];
    let start = "$PoolDescription = '".len();
    let text = &l[start..start + l[start..].find('\'').ok_or("unterminated literal")?];
    let n = text.chars().count();
    if n > LIMIT {
        return Err(format!("{n} characters, the limit is {LIMIT}: {text}"));
    }
    Ok(n)
}

#[test]
fn the_pool_account_description_is_within_new_localuser_limit() {
    let n = description_problem(&script()).unwrap();
    assert!(n <= LIMIT);
}

#[test]
fn a_49_character_description_fails_this_test() {
    // The text that stopped the first real -Apply, put back: the same checker must reject it.
    let old = "AntSpawn pool runner. Nobody holds this password.";
    assert_eq!(old.chars().count(), 49);
    let src = script();
    let cur = "$PoolDescription = 'AntSpawn pool runner. Nobody has the password.'";
    assert_eq!(
        src.matches(cur).count(),
        1,
        "anchor moved; update this test"
    );
    let mutated = src.replace(cur, &format!("$PoolDescription = '{old}'"));
    let e = description_problem(&mutated).unwrap_err();
    assert!(e.contains("49 characters"), "{e}");
    // And an inline literal on the cmdlet (the shape that was shipped) is rejected too.
    let inline = src.replace(
        "-Description $PoolDescription",
        &format!("-Description '{old}'"),
    );
    assert!(description_problem(&inline).is_err());
}

#[test]
fn the_limits_are_checked_before_any_change_and_in_the_dry_run() {
    let src = script();
    let lines = code(&src);
    let check = lines
        .iter()
        .position(|l| l.contains("the cmdlet allows"))
        .expect("the up-front limit check");
    // Before the first Step block or mutating command, and not behind `if ($Apply)`.
    let first_change = lines
        .iter()
        .position(|l| l.starts_with("Step ") || l.contains("New-LocalUser"))
        .unwrap();
    assert!(
        check < first_change,
        "limit check at {check}, first change at {first_change}"
    );
    let before: Vec<&String> = lines[..check]
        .iter()
        .filter(|l| l.starts_with("if ($Apply)") || l.starts_with("if ($Undo)"))
        .collect();
    assert!(
        before.is_empty(),
        "the check must not be gated on -Apply/-Undo: {before:?}"
    );
    // It covers the other string limits the account cmdlets enforce, for every account.
    for needle in [
        "account name",
        "generated password",
        "task name",
        "task -User string",
    ] {
        assert!(src.contains(needle), "{needle}");
    }
}
