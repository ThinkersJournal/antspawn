// SPDX-License-Identifier: MIT OR Apache-2.0
//! Both workspace modes on the REAL restricted account (slice W5 measurement): runner-only,
//! UNVERIFIED until the account is set up and the deployed `spawn-runner.exe` carries sessions.
//! Never `#[ignore]`d. Record real runs in `docs/MEASUREMENTS.md` with date and ref.
//!
//! * `copy-back`: an edit made by the account lands in the real tree ONLY after `apply`, and only for
//!   the reviewed set (known-bad control: before `apply` the real file is unchanged, and a wrong
//!   `set_hash` writes nothing).
//! * `bind`: the account edits the real folder in place; a sibling folder that was NOT granted stays
//!   unreadable to it (known-bad control: if the grant widened the account's reach this would pass the
//!   read), and `close` removes the grant (read back from the journal).
//!
//! The throwaway trees live under the launcher's temp dir. `git status` inside a bind session is
//! asserted to work (ROADMAP row 29).
#![cfg(windows)]

use std::fs;
use std::path::PathBuf;

use antspawn::bind::{Icacls, Journal, pid_alive};
use antspawn::launchers::account::Account;
use antspawn::probes::account;
use antspawn::workspace::{Approvals, Mode, Policy};
use antspawn::workspace_session::{WorkspaceSession, open_bound};
use antspawn::{ExecRequest, Exit, Launcher, Session};

fn out(o: &antspawn::ExecOutcome) -> String {
    String::from_utf8_lossy(o.streams.stdout.as_deref().unwrap_or_default()).into_owned()
}

fn tree(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("antspawn-acctws-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join(".git")).unwrap();
    fs::write(d.join("a.txt"), "one").unwrap();
    d
}

fn ready() -> Option<Account> {
    if !account::set_up() {
        println!(
            "UNVERIFIED: task AntSpawnRunner is not registered; account setup has not been applied here"
        );
        return None;
    }
    let acct = Account::default();
    match acct.open_session() {
        Ok(s) => {
            let _ = s.close();
            Some(acct)
        }
        Err(e) => {
            println!("UNVERIFIED: the deployed spawn-runner cannot host a session yet: {e}");
            None
        }
    }
}

fn run(s: &mut dyn Session, cmd: &str) -> antspawn::ExecOutcome {
    s.exec(&ExecRequest::new(["cmd", "/c", cmd])).unwrap()
}

#[test]
fn copy_back_on_the_real_account_lands_the_reviewed_edit_only_after_apply() {
    let Some(acct) = ready() else { return };
    let real = tree("copy");
    let inner = acct.open_session().unwrap();
    let mut ws = WorkspaceSession::open(inner, &real, Mode::CopyBack, Policy::default())
        .expect("copy the project in as the real account");
    assert_eq!(run(&mut ws, "echo edited> a.txt").exit, Exit::Code(0));
    assert_eq!(run(&mut ws, "echo new> n.txt").exit, Exit::Code(0));
    // Control: the account has edited its copy; the real tree is untouched.
    assert_eq!(fs::read_to_string(real.join("a.txt")).unwrap(), "one");
    assert!(!real.join("n.txt").exists());

    let sum = ws.stage_changes().unwrap();
    assert_eq!(
        sum.changes.modified.keys().collect::<Vec<_>>(),
        ["a.txt"],
        "{sum:?}"
    );
    assert_eq!(
        sum.changes.added.keys().collect::<Vec<_>>(),
        ["n.txt"],
        "{sum:?}"
    );
    let wrong = ws.apply_staged(&"0".repeat(64), &Approvals::default());
    assert!(wrong.is_err());
    assert_eq!(fs::read_to_string(real.join("a.txt")).unwrap(), "one");

    let r = ws
        .apply_staged(&sum.set_hash, &Approvals::default())
        .unwrap();
    assert!(r.is_clean(), "{r:?}");
    assert!(
        fs::read_to_string(real.join("a.txt"))
            .unwrap()
            .contains("edited")
    );
    assert!(real.join("n.txt").is_file());
    assert!(ws.stage_changes().unwrap().changes.is_empty());
    Box::new(ws).close().unwrap();
    let _ = fs::remove_dir_all(&real);
}

/// ROADMAP row 29 follow-up: bind under `C:\Projects` (where the account has an explicit deny that the
/// subtree grant must beat) on a throwaway tree with many protected paths. Prints the open cost (one
/// `icacls` per protected path). Nothing here touches a real project: the tree is made and removed here.
#[test]
fn bind_under_the_projects_folder_beats_the_inherited_deny_and_costs_are_printed() {
    let Some(acct) = ready() else { return };
    let base = PathBuf::from(r"C:\Projects");
    if !base.is_dir() {
        println!(
            "UNVERIFIED: {} does not exist on this machine",
            base.display()
        );
        return;
    }
    let real = base.join(format!("antspawn-bindprobe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&real);
    const PACKAGES: usize = 20;
    fs::create_dir_all(real.join(".git")).unwrap();
    fs::write(real.join("a.txt"), "one").unwrap();
    for i in 0..PACKAGES {
        let d = real.join(format!("pkg{i}"));
        fs::create_dir_all(d.join(".github")).unwrap();
        fs::write(d.join("CLAUDE.md"), "x").unwrap();
        fs::write(d.join("lib.txt"), "y").unwrap();
    }
    let jdir =
        std::env::temp_dir().join(format!("antspawn-acctws-pjournal-{}", std::process::id()));
    let journal = Journal::new(jdir.clone());
    let t0 = std::time::Instant::now();
    let opened = open_bound(
        &acct,
        &real,
        Policy::default(),
        &journal,
        Box::new(Icacls),
        &pid_alive,
    );
    let open_cost = t0.elapsed();
    match opened {
        Err(e) => {
            println!("bind under C:\\Projects REFUSED after {open_cost:?}: {e}");
            let _ = fs::remove_dir_all(&real);
            let _ = fs::remove_dir_all(&jdir);
            panic!("bind under C:\\Projects failed: {e}");
        }
        Ok(mut ws) => {
            // 1 root .git + PACKAGES x (.github + CLAUDE.md) protected paths.
            println!(
                "bind under C:\\Projects: open took {open_cost:?} for {} protected paths",
                1 + 2 * PACKAGES
            );
            let w = run(&mut ws, "echo edited> a.txt");
            assert_eq!(w.exit, Exit::Code(0), "{}", out(&w));
            assert!(
                fs::read_to_string(real.join("a.txt"))
                    .unwrap()
                    .contains("edited"),
                "the grant did not beat the inherited deny: the edit never reached the folder"
            );
            // Control: a protected file stays unwritable.
            let bad = run(&mut ws, "echo pwn> pkg0\\CLAUDE.md");
            assert_ne!(bad.exit, Exit::Code(0), "wrote a protected file");
            assert_eq!(
                fs::read_to_string(real.join("pkg0").join("CLAUDE.md")).unwrap(),
                "x"
            );
            let t1 = std::time::Instant::now();
            Box::new(ws).close().unwrap();
            println!("bind under C:\\Projects: close took {:?}", t1.elapsed());
        }
    }
    assert!(
        journal.entries().is_empty(),
        "close left a grant journal entry"
    );
    let _ = fs::remove_dir_all(&real);
    let _ = fs::remove_dir_all(&jdir);
}

#[test]
fn bind_on_the_real_account_edits_in_place_keeps_siblings_out_and_removes_the_grant() {
    let Some(acct) = ready() else { return };
    let real = tree("bind");
    // A real repository, so the git probe below is about the account and not a fake `.git`.
    fs::remove_dir_all(real.join(".git")).unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&real)
        .status();
    println!("git init in the bind tree: {init:?}");
    let sibling = tree("sibling");
    fs::write(sibling.join("secret.txt"), "not yours").unwrap();
    let jdir = std::env::temp_dir().join(format!("antspawn-acctws-journal-{}", std::process::id()));
    let journal = Journal::new(jdir.clone());
    let mut ws = open_bound(
        &acct,
        &real,
        Policy::default(),
        &journal,
        Box::new(Icacls),
        &pid_alive,
    )
    .expect("grant the account the project folder and open a session in it");
    assert_eq!(ws.mode(), Mode::Bind);
    assert_eq!(run(&mut ws, "echo edited> a.txt").exit, Exit::Code(0));
    assert!(
        fs::read_to_string(real.join("a.txt"))
            .unwrap()
            .contains("edited"),
        "an edit by the account did not reach the real folder"
    );
    // Control: a folder that was not granted stays closed to the account.
    let read = run(
        &mut ws,
        &format!("type \"{}\"", sibling.join("secret.txt").display()),
    );
    assert_ne!(
        read.exit,
        Exit::Code(0),
        "the account read an ungranted sibling: {}",
        out(&read)
    );
    // ROADMAP row 29 (fixed 2026-11-17): the deny on `.git` must not carry SYNCHRONIZE (else the
    // account cannot list it and git searches upward), and the bind session supplies `safe.directory`
    // (else "dubious ownership"). The tree is a real repository made by the owner before the grant.
    let git = run(&mut ws, "git status --porcelain 2>&1");
    assert_eq!(git.exit, Exit::Code(0), "git status failed: {}", out(&git));
    assert!(out(&git).contains("a.txt"), "{}", out(&git));
    Box::new(ws).close().unwrap();
    assert!(
        journal.entries().is_empty(),
        "close left a grant journal entry"
    );
    let _ = fs::remove_dir_all(&real);
    let _ = fs::remove_dir_all(&sibling);
    let _ = fs::remove_dir_all(&jdir);
}
