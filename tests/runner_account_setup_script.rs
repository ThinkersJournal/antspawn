// SPDX-License-Identifier: MIT OR Apache-2.0
//! `scripts/runner-account-setup.ps1` failed on its first real run (2026-10-31): `New-LocalUser -Description`
//! takes at most 48 characters and ours had 59, and the thrown error left `$LASTEXITCODE` at 0. These tests
//! pin both. They do not apply the script (that needs an elevated shell and is CireSnave's).

use std::path::PathBuf;

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/runner-account-setup.ps1")
}

fn script() -> String {
    std::fs::read_to_string(script_path()).unwrap()
}

/// The text of a PowerShell single- or double-quoted literal that starts at `s`, if it does.
fn literal(s: &str) -> Option<String> {
    let q = s.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let rest = &s[1..];
    Some(rest[..rest.find(q)?].to_string())
}

/// Every value given to `-Description`, with a `$Variable` resolved from its `$Variable = '...'` line and
/// `$Name` replaced by the longest name the script accepts (16 characters).
fn descriptions(src: &str) -> Vec<String> {
    let mut out = vec![];
    // Comment lines may mention the parameter; only code lines count.
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    let src = code.as_str();
    for (at, _) in src.match_indices("-Description ") {
        let arg = src[at + "-Description ".len()..].trim_start();
        let text = if let Some(var) = arg.strip_prefix('$') {
            let var: String = var
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let def = format!("${var} = ");
            let line = src
                .lines()
                .find(|l| l.trim_start().starts_with(&def))
                .unwrap_or_else(|| panic!("no `{def}...` line for -Description ${var}"));
            literal(line.trim_start()[def.len()..].trim_start())
                .unwrap_or_else(|| panic!("`{def}` is not a quoted literal"))
        } else {
            literal(arg)
                .unwrap_or_else(|| panic!("-Description is neither a literal nor a variable"))
        };
        out.push(text.replace("$Name", "0123456789abcdef"));
    }
    out
}

#[test]
fn every_local_user_description_fits_new_localuser_s_48_characters() {
    let d = descriptions(&script());
    assert!(
        !d.is_empty(),
        "the script no longer sets a -Description; update this test"
    );
    for t in &d {
        assert!(
            t.chars().count() <= 48,
            "{} chars (limit 48): {t}",
            t.chars().count()
        );
    }
}

#[test]
fn the_description_check_catches_the_text_that_failed_for_real() {
    // Control: the 59-character text from the first run, in the script's old inline form, must be flagged.
    let old = r#"New-LocalUser -Name x -Description "AntSpawn Actions runner $Name. Nobody holds this password." | Out-Null"#;
    let d = descriptions(old);
    assert_eq!(d.len(), 1);
    assert!(d[0].chars().count() > 48, "{}", d[0].chars().count());
}

#[cfg(windows)]
#[test]
fn a_thrown_error_ends_the_script_with_a_nonzero_exit_code_even_when_called_with_ampersand() {
    // BinDir does not exist, so the script throws "spawn-jit-shim.exe not found" (a dry run: nothing changes).
    // Called with `&` from a session, as CireSnave did, an uncaught throw left $LASTEXITCODE at 0.
    let cmd = format!(
        "& '{}' -Name t1 -BinDir 'C:\\no-such-antspawn-dir' -RunnerSource 'C:\\no-such-antspawn-dir'; exit $LASTEXITCODE",
        script_path().display()
    );
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &cmd,
        ])
        .output()
        .expect("powershell.exe");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("not found"),
        "the script did not reach its own check: {text}"
    );
    assert!(text.contains("FAILED:"), "{text}");
    assert_eq!(out.status.code(), Some(1), "{text}");
}
