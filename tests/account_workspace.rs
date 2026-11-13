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
//! only printed, never asserted: whether the account can run git there is not yet known.
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
    // Not asserted: can the account run git here? (never measured before). The tree is a real
    // repository (`git init` by the owner before the grant), not the empty `.git` folder `tree` makes.
    let git = run(&mut ws, "git status --porcelain 2>&1");
    println!(
        "git status in a bind session: exit {:?}, output {:?}",
        git.exit,
        out(&git).trim()
    );
    // Also only printed: does a ceiling at the project's parent (the folder the account cannot read) help?
    let mut ceil = ExecRequest::new(["git", "status", "--porcelain"]);
    ceil.env.insert(
        "GIT_CEILING_DIRECTORIES".into(),
        real.parent().unwrap().display().to_string(),
    );
    let c = ws.exec(&ceil).unwrap();
    println!(
        "git status with GIT_CEILING_DIRECTORIES=<parent>: exit {:?}, output {:?}, stderr {:?}",
        c.exit,
        out(&c).trim(),
        String::from_utf8_lossy(c.streams.stderr.as_deref().unwrap_or_default()).trim()
    );
    Box::new(ws).close().unwrap();
    assert!(
        journal.entries().is_empty(),
        "close left a grant journal entry"
    );
    let _ = fs::remove_dir_all(&real);
    let _ = fs::remove_dir_all(&sibling);
    let _ = fs::remove_dir_all(&jdir);
}
