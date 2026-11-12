// SPDX-License-Identifier: MIT OR Apache-2.0
//! `scripts/broker-account.ps1` (slice V7) has NEVER been applied: it is CireSnave's elevated paste. These tests
//! pin what can be checked as text without elevation: the `-Description` limit that broke
//! `runner-account-setup.ps1` on its first real run, and the things the script promises it does not do. They
//! do not show the script works; only a real `-Apply` can.

fn script() -> String {
    std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/broker-account.ps1"),
    )
    .unwrap()
}

/// Lines of code: comments, the `<# #>` help block and blank lines removed.
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

#[test]
fn the_account_description_fits_new_localuser_limit_of_48_characters() {
    let src = script();
    let lines = code(&src);
    let found: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("-Description "))
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    let l = found[0];
    let start = l.find("-Description '").expect("a single-quoted literal") + "-Description '".len();
    let text = &l[start..start + l[start..].find('\'').unwrap()];
    assert!(
        text.chars().count() <= 48,
        "{} chars: {text}",
        text.chars().count()
    );
    // Control: the 59-character text that failed on 2026-10-31 is over the limit this test enforces.
    assert!(
        "AntSpawn runner account (restricted, runs one job at a time)"
            .chars()
            .count()
            > 48
    );
}

#[test]
fn it_never_starts_the_broker_adds_a_firewall_rule_or_writes_the_config() {
    let src = script();
    let lines = code(&src);
    for forbidden in [
        "Start-ScheduledTask",
        "New-NetFirewallRule",
        "Set-Content",
        "Out-File",
        "New-VM",
        "New-VMSwitch",
        "Add-LocalGroupMember -Group 'Administrators'",
    ] {
        let hits: Vec<&String> = lines
            .iter()
            // the closing "Next" message names Start-ScheduledTask for the operator, it does not run it
            .filter(|l| l.contains(forbidden) && !l.starts_with("Write-Host"))
            .collect();
        // Set-Content is allowed only for the temporary secedit .inf of the rights helper.
        let hits: Vec<_> = hits
            .into_iter()
            .filter(|l| !(forbidden == "Set-Content" && l.contains("$inf")))
            .collect();
        assert!(hits.is_empty(), "{forbidden}: {hits:?}");
    }
    // Control: the filter does see real commands (the script does register a task and call icacls).
    assert!(lines.iter().any(|l| l.contains("Register-ScheduledTask")));
    assert!(lines.iter().any(|l| l.contains("icacls.exe")));
}

#[test]
fn dry_run_is_the_default_and_every_change_goes_through_step() {
    let src = script();
    assert!(src.contains("[switch]$Apply"));
    assert!(src.contains(
        "if ($Apply) { Write-Host \"APPLY  $What\"; & $Do } else { Write-Host \"WOULD  $What\" }"
    ));
    let lines = code(&src);
    for cmd in [
        "New-LocalUser",
        "Remove-LocalUser",
        "Register-ScheduledTask",
        "Remove-Item -Recurse",
    ] {
        let hits: Vec<usize> = (0..lines.len())
            .filter(|i| lines[*i].contains(cmd))
            .collect();
        assert!(!hits.is_empty(), "{cmd}: the script no longer has it");
        // EVERY occurrence sits inside a Step block: an earlier (or the same) line opens `Step "..." {`
        // and no lone `}` closes it before the command.
        for first in hits {
            let opened = lines[..=first].iter().rposition(|l| l.starts_with("Step "));
            let ok = opened.is_some_and(|o| !lines[o..first].iter().any(|l| l == "}"));
            assert!(ok, "{cmd} (code line {first}) is outside any Step block");
        }
    }
}
