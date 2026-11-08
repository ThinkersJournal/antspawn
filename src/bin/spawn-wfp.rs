// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-wfp`: Part A of the account network boundary (see `antspawn::wfp` and `docs/ACCOUNT-ISOLATION.md`).
//! Run by the machine owner in an elevated shell, never by a launcher or a lane.
//!
//! * `plan   [--account NAME | --sid SID]`: print exactly what `apply` would add. Changes nothing.
//! * `status [--account NAME | --sid SID]`: read our two filters back. Changes nothing.
//! * `apply  [--account NAME | --sid SID]`: add the provider, sublayer and two filters in one transaction,
//!   then read them back; a mismatch removes them again. Needs an elevated token.
//! * `undo`: remove exactly our filters, sublayer and provider (fixed keys), then read back that they are gone.
//!
//! The default account is `antspawn-run`. Exit 0 = success, 1 = a failure (the reason is printed), 2 = usage.

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    use antspawn::wfp;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        eprintln!("usage: spawn-wfp plan|status|apply|undo [--account NAME | --sid SID]");
        return ExitCode::from(2);
    };
    let mut who = "antspawn-run".to_string();
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--account" | "--sid" => match it.next() {
                Some(v) => who = v.clone(),
                None => {
                    eprintln!("{a} needs a value");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("unknown argument `{other}`");
                return ExitCode::from(2);
            }
        }
    }
    let result = match cmd {
        "plan" => wfp::resolve_sid(&who)
            .and_then(|sid| wfp::Plan::build(&sid))
            .map(|p| {
                format!(
                    "DRY RUN, nothing is changed. `apply` would add:\n{}",
                    p.describe()
                )
            }),
        "status" => wfp::resolve_sid(&who).and_then(|sid| wfp::status(&sid)),
        "apply" => wfp::resolve_sid(&who).and_then(|sid| wfp::apply(&sid)),
        "undo" => wfp::undo(),
        other => {
            eprintln!("unknown command `{other}`");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spawn-wfp: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("spawn-wfp is Windows only");
    ExitCode::from(2)
}
