// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Get-TaskSdProblems` (scripts/AntSpawnTaskSd.ps1) against the REAL descriptor Task Scheduler gave
//! `AntSpawnRunner-spawn1` after CireSnave's first `-Apply` (2026-10-31, read back with
//! `GetSecurityDescriptor(4)`; the SID values are from that machine). The old check matched the text `GRGX`
//! and so failed a correct descriptor, because Windows stores and prints the mask as 0x1200a9.
#![cfg(windows)]

use std::path::PathBuf;

const SCRIPT: &str = r#"
. '{MODULE}'
$L = 'S-1-5-21-80910236-1237618238-2053816910-1001'   # launcher (cires)
$R = 'S-1-5-21-80910236-1237618238-2053816910-1021'   # run-as account (ghr-spawn1)
$real = 'D:(A;;FA;;;BA)(A;;FA;;;SY)(A;;0x1200a9;;;' + $L + ')(A;ID;0x1f019f;;;BA)(A;ID;0x1f019f;;;SY)(A;ID;FA;;;BA)(A;;FR;;;' + $R + ')'
function N($sddl) { @(Get-TaskSdProblems -Sddl $sddl -LauncherSid $L -RunAsSid $R).Count }
"real=" + (N $real)
"old_regex_matches_real=" + [bool](($real -match 'GRGX') -and ($real -match [regex]::Escape($L)))
"extra_principal=" + (N ($real + '(A;;FA;;;WD)'))
"launcher_can_write=" + (N $real.Replace('0x1200a9', '0x1201bf'))
"launcher_can_change_permissions=" + (N $real.Replace('0x1200a9', '0x1600a9'))
"launcher_missing=" + (N $real.Replace('(A;;0x1200a9;;;' + $L + ')', ''))
"runas_can_write=" + (N $real.Replace('FR;;;' + $R, 'FA;;;' + $R))
"runas_absent=" + (N $real.Replace('(A;;FR;;;' + $R + ')', ''))
"deny_entry=" + (N ($real + '(D;;FA;;;' + $R + ')'))
"#;

fn run() -> std::collections::HashMap<String, String> {
    let module = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/AntSpawnTaskSd.ps1");
    let cmd = SCRIPT.replace("{MODULE}", &module.display().to_string());
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
    let m: std::collections::HashMap<String, String> = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    assert!(m.contains_key("real"), "the driver did not run: {text}");
    m
}

#[test]
fn the_real_descriptor_passes_and_the_old_text_match_would_have_failed_it() {
    let m = run();
    assert_eq!(m["real"], "0", "a correct descriptor must have no problems");
    assert_eq!(
        m["old_regex_matches_real"], "False",
        "control: the old GRGX text match fails this same, correct descriptor"
    );
}

#[test]
fn every_way_of_being_wrong_is_a_problem() {
    let m = run();
    for k in [
        "extra_principal",
        "launcher_can_write",
        "launcher_can_change_permissions",
        "launcher_missing",
        "runas_can_write",
        "deny_entry",
    ] {
        assert_ne!(m[k], "0", "{k} must be reported: {m:?}");
    }
    assert_eq!(
        m["runas_absent"], "0",
        "the run-as entry is Task Scheduler's and may be absent"
    );
}
