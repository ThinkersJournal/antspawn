// SPDX-License-Identifier: MIT OR Apache-2.0
//! `scripts/guest-bootstrap.ps1` has NEVER run: it needs a rebaked image. These tests pin, as text, what the PM ruling
//! (2026-11-26, no long-lived guest credential) needs of it: it reads the item the host script puts on the VM bus, sets the
//! password without ever printing, logging, saving or exporting it, and uses no network. They do not show it works.

fn read(p: &str) -> String {
    std::fs::read_to_string(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(p)).unwrap()
}

/// Code lines: the `<# #>` block, comments and blanks removed.
fn code(src: &str) -> String {
    let mut in_help = false;
    let mut out = String::new();
    for l in src.lines() {
        let t = l.trim();
        if t.starts_with("<#") {
            in_help = true;
        }
        if !in_help && !t.is_empty() && !t.starts_with('#') {
            out.push_str(t);
            out.push('\n');
        }
        if t.ends_with("#>") {
            in_help = false;
        }
    }
    out
}

/// What the script must not contain: anything that prints, saves, exports or sends a value.
fn problem(src: &str) -> Result<(), String> {
    let c = code(src);
    for bad in [
        "Write-Host",
        "Write-Output",
        "Write-Error",
        "Write-Verbose",
        "Write-Debug",
        "Out-File",
        "Set-Content",
        "Add-Content",
        "Tee-Object",
        "Start-Transcript",
        "$env:",
        "[Environment]::",
        "Invoke-WebRequest",
        "Invoke-RestMethod",
        "Net.WebClient",
        "Test-NetConnection",
        "-AsPlainText",
        "ConvertFrom-SecureString",
        "net.exe",
        "net user",
        "Start-Process",
        "-ArgumentList",
    ] {
        if c.contains(bad) {
            return Err(format!("`{bad}` in the bootstrap code"));
        }
    }
    if !c.contains("Set-LocalUser -Name 'Administrator' -Password $ss") {
        return Err("it does not set the Administrator password from a SecureString".into());
    }
    Ok(())
}

#[test]
fn the_item_name_matches_the_one_the_host_sets() {
    let host = read("src/adapter/vm_backend.rs");
    let name = host
        .lines()
        .find(|l| l.starts_with("pub const KVP_NAME"))
        .and_then(|l| l.split('"').nth(1))
        .expect("KVP_NAME");
    assert!(code(&read("scripts/guest-bootstrap.ps1")).contains(&format!("$name = '{name}'")));
}

#[test]
fn the_bootstrap_never_prints_saves_exports_or_sends_the_password() {
    problem(&read("scripts/guest-bootstrap.ps1")).unwrap();
}

/// Known-bad controls: each of these must be refused by the same check.
#[test]
fn control_a_bootstrap_that_leaks_is_refused() {
    let good = read("scripts/guest-bootstrap.ps1");
    for leak in [
        "Write-Host $value",
        "Set-Content pw.txt $value",
        "$env:PW = $value",
        "net user Administrator $value",
        "ConvertTo-SecureString $value -AsPlainText -Force",
    ] {
        let bad = format!("{good}\n{leak}\n");
        assert!(problem(&bad).is_err(), "{leak} was not caught");
    }
    assert!(
        problem(&good.replace("Set-LocalUser -Name 'Administrator' -Password $ss", "")).is_err()
    );
}
