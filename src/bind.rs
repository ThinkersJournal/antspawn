// SPDX-License-Identifier: MIT OR Apache-2.0
//! `bind` workspace mode (slice W4 of `docs/SESSION-PLAN.md`): an ACL grant on the real project.
//!
//! The sandbox account is given Modify on the project folder for the session's life and the grant is
//! taken away at close. W0 (`docs/MEASUREMENTS.md`, 2026-11-06) measured the two facts this rests on:
//! an explicit grant on the subtree beats the inherited deny on `C:\Projects`, and an explicit deny on
//! a subpath beats the grant, provided the deny uses exact rights (no SYNCHRONIZE, or the path becomes
//! unreadable). So protected paths that exist at open get an exact-rights deny of every write.
//!
//! Safety, in the order it matters:
//! * Denies are applied **before** the grant, so there is never a moment where a protected path is
//!   writable.
//! * A journal entry (launcher-user-owned, outside the project) is written **before** the first ACL
//!   change and removed only after removal is read back, so a launcher that dies mid-session leaves a
//!   record [`reclaim`] can act on. Reclaim is idempotent.
//! * Removal is read back, never trusted from an exit code: [`BindGrant::revoke`] fails, loudly, if the
//!   account is still named on the root or on a denied path.
//! * Two sessions on one root (or one inside the other) are refused while the first owner is alive.
//!
//! Not covered, stated rather than implied: a protected path the account creates after open (it was not
//! there to deny; [`BindGrant::new_protected_paths`] reports it so the harness sees it); everything the
//! account wrote is as trusted as the account, so a host-side run of it (tests, hooks, CI) is an escape.
//! Privileged work is never done here: the launcher's user owns the project and edits its own ACLs.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::workspace::{Policy, is_reparse};

/// The ACL operations a bind needs. The real one is [`Icacls`]; tests use a recording fake.
pub trait Acl {
    /// Explicit Modify for `who` on `path`, inherited by everything beneath it.
    fn grant_modify(&self, path: &Path, who: &str) -> Result<(), String>;
    /// Explicit deny of every write (exact rights, never SYNCHRONIZE) for `who` on `path`.
    fn deny_writes(&self, path: &Path, who: &str, is_dir: bool) -> Result<(), String>;
    /// Remove every explicit entry for `who` on `path` and beneath it.
    fn remove(&self, path: &Path, who: &str) -> Result<(), String>;
    /// Whether `who` is still named in `path`'s access list (read back, not assumed).
    fn mentions(&self, path: &Path, who: &str) -> Result<bool, String>;
}

/// `icacls.exe`, run as the invoking user (the project's owner).
#[derive(Debug, Default, Clone, Copy)]
pub struct Icacls;

fn icacls(args: &[&std::ffi::OsStr]) -> Result<String, String> {
    let out = Command::new("icacls.exe")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run icacls.exe: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(text)
    } else {
        Err(format!(
            "icacls failed ({}): {} {}",
            out.status,
            text.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

impl Acl for Icacls {
    fn grant_modify(&self, path: &Path, who: &str) -> Result<(), String> {
        let rule = format!("{who}:(OI)(CI)M");
        icacls(&[path.as_os_str(), "/grant".as_ref(), rule.as_ref()]).map(drop)
    }

    fn deny_writes(&self, path: &Path, who: &str, is_dir: bool) -> Result<(), String> {
        // WriteData, AppendData, WriteExtendedAttributes, WriteAttributes, Delete,
        // DeleteSubdirectoriesAndFiles: what W0 measured. Not `W`, which adds SYNCHRONIZE.
        let inherit = if is_dir { "(OI)(CI)" } else { "" };
        let rule = format!("{who}:{inherit}(WD,AD,WEA,WA,D,DC)");
        icacls(&[path.as_os_str(), "/deny".as_ref(), rule.as_ref()]).map(drop)
    }

    fn remove(&self, path: &Path, who: &str) -> Result<(), String> {
        for kind in ["/remove:g", "/remove:d"] {
            icacls(&[
                path.as_os_str(),
                kind.as_ref(),
                who.as_ref(),
                "/T".as_ref(),
                "/C".as_ref(),
            ])?;
        }
        Ok(())
    }

    fn mentions(&self, path: &Path, who: &str) -> Result<bool, String> {
        let text = icacls(&[path.as_os_str()])?;
        let who = who.to_ascii_lowercase();
        Ok(text.lines().any(|l| {
            let l = l.to_ascii_lowercase();
            // `<path> DOMAIN\who:(M)` on the first line, `    DOMAIN\who:(M)` after it.
            l.contains(&format!("\\{who}:")) || l.trim_start().starts_with(&format!("{who}:"))
        }))
    }
}

/// A principal name that is safe to put in an `icacls` rule.
pub fn valid_principal(who: &str) -> bool {
    !who.is_empty()
        && who.len() <= 64
        && who
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '$'))
}

/// What the launcher keeps about a live bind so a later launcher can undo it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub root: PathBuf,
    pub who: String,
    pub denied: Vec<PathBuf>,
    /// The launcher process that owns the bind.
    pub pid: u32,
}

/// The directory of [`Entry`] files. It belongs to the launcher's user, never the account.
#[derive(Debug, Clone)]
pub struct Journal {
    dir: PathBuf,
}

static IDS: AtomicU64 = AtomicU64::new(0);

impl Journal {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Journal { dir: dir.into() }
    }

    /// `%LOCALAPPDATA%\antspawn\grants`.
    pub fn default_dir() -> Result<Self, String> {
        let base = std::env::var_os("LOCALAPPDATA")
            .ok_or("LOCALAPPDATA is not set; cannot place the grant journal")?;
        Ok(Journal::new(
            PathBuf::from(base).join("antspawn").join("grants"),
        ))
    }

    fn write(&self, e: &Entry) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.dir).map_err(|x| format!("{}: {x}", self.dir.display()))?;
        let name = format!(
            "{}-{}-{}.toml",
            e.pid,
            IDS.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let path = self.dir.join(name);
        let text = toml::to_string(e).map_err(|x| x.to_string())?;
        fs::write(&path, text).map_err(|x| format!("{}: {x}", path.display()))?;
        Ok(path)
    }

    /// Every entry on file; an unreadable one is returned as an error beside its path, never skipped.
    pub fn entries(&self) -> Vec<(PathBuf, Result<Entry, String>)> {
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut v: Vec<_> = rd
            .filter_map(Result::ok)
            .map(|d| d.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .map(|p| {
                let e = fs::read_to_string(&p)
                    .map_err(|x| x.to_string())
                    .and_then(|t| toml::from_str::<Entry>(&t).map_err(|x| x.to_string()));
                (p, e)
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

fn norm(p: &Path) -> String {
    p.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn overlaps(a: &Path, b: &Path) -> bool {
    let (a, b) = (norm(a), norm(b));
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

/// Remove what `e` granted and read it back.
fn undo(e: &Entry, acl: &dyn Acl) -> Result<(), String> {
    // A path the project no longer has holds no entry to remove; the root must still exist.
    let mut paths: Vec<&Path> = vec![e.root.as_path()];
    paths.extend(e.denied.iter().map(PathBuf::as_path));
    let mut problems = Vec::new();
    for p in &paths {
        if p.exists()
            && let Err(x) = acl.remove(p, &e.who)
        {
            problems.push(x);
        }
    }
    for p in &paths {
        if p.exists() {
            match acl.mentions(p, &e.who) {
                Ok(false) => {}
                Ok(true) => problems.push(format!(
                    "`{}` still names `{}` after removal",
                    p.display(),
                    e.who
                )),
                Err(x) => problems.push(format!("cannot read back `{}`: {x}", p.display())),
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Undo the binds whose owner is gone. `alive` says whether a launcher pid still runs. One result per
/// entry acted on; an entry whose removal could not be proven stays on file for the next attempt.
pub fn reclaim(
    journal: &Journal,
    acl: &dyn Acl,
    alive: &dyn Fn(u32) -> bool,
) -> Vec<(PathBuf, Result<(), String>)> {
    let mut done = Vec::new();
    for (path, e) in journal.entries() {
        match e {
            Err(x) => done.push((path, Err(format!("unreadable journal entry: {x}")))),
            Ok(e) if alive(e.pid) => {
                let _ = e;
            }
            Ok(e) => {
                let r = undo(&e, acl);
                if r.is_ok() {
                    let _ = fs::remove_file(&path);
                }
                done.push((path, r));
            }
        }
    }
    done
}

/// Whether a process with this pid exists (`tasklist`; a failure to ask counts as alive, the safe
/// answer: a live owner's grant is never reclaimed from under it).
pub fn pid_alive(pid: u32) -> bool {
    let Ok(out) = Command::new("tasklist.exe")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
    else {
        return true;
    };
    if !out.status.success() {
        return true;
    }
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

/// A live bind. Dropping it revokes (best effort); call [`BindGrant::revoke`] to learn whether the
/// removal was proven.
pub struct BindGrant {
    entry: Entry,
    journal_file: PathBuf,
    acl: Box<dyn Acl>,
    at_open: BTreeSet<String>,
    policy: Policy,
    done: bool,
}

impl BindGrant {
    /// Deny the protected paths that exist, then grant Modify on `root`.
    pub fn open(
        root: &Path,
        who: &str,
        policy: &Policy,
        journal: &Journal,
        acl: Box<dyn Acl>,
        alive: &dyn Fn(u32) -> bool,
    ) -> Result<BindGrant, String> {
        if !valid_principal(who) {
            return Err(format!("`{who}` is not a plain account name"));
        }
        let md = fs::symlink_metadata(root).map_err(|e| format!("{}: {e}", root.display()))?;
        if !md.is_dir() || is_reparse(&md) {
            return Err(format!(
                "{}: a bind root must be a real directory, not a link",
                root.display()
            ));
        }
        if root.parent().is_none() {
            return Err("a bind root must not be a drive root".into());
        }
        // Another live bind on an overlapping tree is refused; a dead owner's is reclaimed first.
        for (p, e) in journal.entries() {
            match e {
                Ok(e) if overlaps(&e.root, root) && alive(e.pid) => {
                    return Err(format!(
                        "`{}` is already bound by launcher pid {} ({}); two sessions on one root are refused",
                        e.root.display(),
                        e.pid,
                        p.display()
                    ));
                }
                Ok(e) if overlaps(&e.root, root) => {
                    undo(&e, acl.as_ref()).map_err(|x| {
                        format!("a dead launcher's bind on this root could not be removed: {x}")
                    })?;
                    let _ = fs::remove_file(&p);
                }
                _ => {}
            }
        }
        let mut denied = Vec::new();
        find_protected(root, "", policy, &mut denied)?;
        let at_open: BTreeSet<String> = denied
            .iter()
            .filter_map(|p| p.strip_prefix(root).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        let entry = Entry {
            root: root.to_path_buf(),
            who: who.to_owned(),
            denied,
            pid: std::process::id(),
        };
        // Journal first: from here a crash leaves something to reclaim.
        let journal_file = journal.write(&entry)?;
        let mut g = BindGrant {
            entry,
            journal_file,
            acl,
            at_open,
            policy: policy.clone(),
            done: false,
        };
        if let Err(e) = g.apply() {
            let undone = g.revoke_inner();
            return Err(match undone {
                Ok(()) => e,
                Err(u) => format!("{e}; and the partial grant could not be removed: {u}"),
            });
        }
        Ok(g)
    }

    fn apply(&mut self) -> Result<(), String> {
        for p in &self.entry.denied {
            self.acl.deny_writes(p, &self.entry.who, p.is_dir())?;
        }
        self.acl.grant_modify(&self.entry.root, &self.entry.who)
    }

    pub fn root(&self) -> &Path {
        &self.entry.root
    }

    /// Protected paths that exist now and did not at open: the account made them, so nothing denies
    /// it writing there. The harness should treat them as untrusted.
    pub fn new_protected_paths(&self) -> Result<Vec<String>, String> {
        let mut now = Vec::new();
        find_protected(&self.entry.root, "", &self.policy, &mut now)?;
        Ok(now
            .iter()
            .filter_map(|p| p.strip_prefix(&self.entry.root).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .filter(|p| !self.at_open.contains(p))
            .collect())
    }

    /// Take the grant away and prove it: an error if the account is still on the root or a denied path.
    pub fn revoke(mut self) -> Result<(), String> {
        self.revoke_inner()
    }

    fn revoke_inner(&mut self) -> Result<(), String> {
        if self.done {
            return Ok(());
        }
        let r = undo(&self.entry, self.acl.as_ref());
        if r.is_ok() {
            self.done = true;
            let _ = fs::remove_file(&self.journal_file);
        }
        r
    }
}

impl Drop for BindGrant {
    fn drop(&mut self) {
        // On failure the journal entry stays, so `reclaim` retries.
        let _ = self.revoke_inner();
    }
}

/// Protected paths under `root` (files and directories), not descending into one. A link anywhere is
/// an error: a deny on a link would not protect what it points to.
fn find_protected(
    root: &Path,
    rel: &str,
    policy: &Policy,
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let rd = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut names: Vec<String> = rd
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    names.sort();
    for name in names {
        let child = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let abs = dir.join(&name);
        let md = fs::symlink_metadata(&abs).map_err(|e| format!("{}: {e}", abs.display()))?;
        if is_reparse(&md) {
            return Err(format!(
                "`{child}` is a symlink or reparse point; a bind cannot protect what it points to"
            ));
        }
        if policy.is_protected(&child) {
            out.push(abs);
        } else if md.is_dir() {
            find_protected(root, &child, policy, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default, Clone)]
    struct Fake {
        log: Rc<RefCell<Vec<String>>>,
        /// Paths that still "mention" the account after removal (a removal that did not take).
        sticky: Rc<RefCell<BTreeSet<String>>>,
        state: Rc<RefCell<BTreeSet<String>>>,
        fail_grant: bool,
    }

    impl Acl for Fake {
        fn grant_modify(&self, p: &Path, _: &str) -> Result<(), String> {
            if self.fail_grant {
                return Err("grant refused".into());
            }
            self.log.borrow_mut().push(format!("grant {}", p.display()));
            self.state.borrow_mut().insert(norm(p));
            Ok(())
        }
        fn deny_writes(&self, p: &Path, _: &str, _: bool) -> Result<(), String> {
            self.log.borrow_mut().push(format!("deny {}", p.display()));
            self.state.borrow_mut().insert(norm(p));
            Ok(())
        }
        fn remove(&self, p: &Path, _: &str) -> Result<(), String> {
            self.log
                .borrow_mut()
                .push(format!("remove {}", p.display()));
            if !self.sticky.borrow().contains(&norm(p)) {
                self.state.borrow_mut().remove(&norm(p));
            }
            Ok(())
        }
        fn mentions(&self, p: &Path, _: &str) -> Result<bool, String> {
            Ok(self.state.borrow().contains(&norm(p)))
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antspawn-bind-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn project(tag: &str) -> (PathBuf, Journal) {
        let root = dir(&format!("{tag}-root"));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("sub/.github")).unwrap();
        fs::write(root.join("CLAUDE.md"), "x").unwrap();
        fs::write(root.join("src/a.rs"), "x").unwrap();
        (root, Journal::new(dir(&format!("{tag}-journal"))))
    }

    fn open(root: &Path, j: &Journal, f: &Fake) -> Result<BindGrant, String> {
        BindGrant::open(
            root,
            "antspawn-run",
            &Policy::default(),
            j,
            Box::new(f.clone()),
            &|_| true,
        )
    }

    #[test]
    fn denies_come_before_the_grant_and_cover_every_protected_path() {
        let (root, j) = project("order");
        let f = Fake::default();
        let g = open(&root, &j, &f).unwrap();
        let log = f.log.borrow().clone();
        let last = log.last().unwrap();
        assert!(last.starts_with("grant "), "{log:?}");
        let denied: Vec<_> = log.iter().filter(|l| l.starts_with("deny ")).collect();
        assert_eq!(denied.len(), 3, "{log:?}"); // .git, sub/.github, CLAUDE.md
        assert!(log.iter().position(|l| l.starts_with("deny ")).unwrap() < log.len() - 1);
        assert!(
            !log.iter().any(|l| l.contains("src")),
            "src is not protected"
        );
        assert_eq!(j.entries().len(), 1, "the journal is written");
        g.revoke().unwrap();
        assert!(j.entries().is_empty(), "revoke clears the journal");
    }

    #[test]
    fn revoke_is_read_back_and_a_removal_that_did_not_take_is_an_error_that_keeps_the_journal() {
        let (root, j) = project("sticky");
        let f = Fake::default();
        let g = open(&root, &j, &f).unwrap();
        f.sticky.borrow_mut().insert(norm(&root));
        let e = g.revoke().unwrap_err();
        assert!(e.contains("still names"), "{e}");
        assert_eq!(
            j.entries().len(),
            1,
            "an unproven removal stays reclaimable"
        );
    }

    #[test]
    fn a_failed_grant_is_undone_and_leaves_no_journal() {
        let (root, j) = project("failgrant");
        let f = Fake {
            fail_grant: true,
            ..Fake::default()
        };
        let e = open(&root, &j, &f).err().unwrap();
        assert_eq!(e, "grant refused");
        assert!(f.state.borrow().is_empty(), "the denies were removed again");
        assert!(j.entries().is_empty());
    }

    #[test]
    fn a_crashed_launchers_grant_is_reclaimed_and_a_live_ones_is_not() {
        let (root, j) = project("crash");
        let f = Fake::default();
        let g = open(&root, &j, &f).unwrap();
        std::mem::forget(g); // the launcher dies: no Drop, no revoke
        assert!(!f.state.borrow().is_empty());
        // Control: while the owner lives, reclaim leaves it alone.
        let r = reclaim(&j, &f, &|_| true);
        assert!(r.is_empty());
        assert!(!f.state.borrow().is_empty());
        // Owner gone:
        let r = reclaim(&j, &f, &|_| false);
        assert_eq!(r.len(), 1);
        assert!(r[0].1.is_ok());
        assert!(f.state.borrow().is_empty(), "nothing names the account");
        assert!(j.entries().is_empty());
    }

    #[test]
    fn two_sessions_on_one_root_or_nested_roots_are_refused_while_the_owner_lives() {
        let (root, j) = project("two");
        let f = Fake::default();
        let _g = open(&root, &j, &f).unwrap();
        let e = open(&root, &j, &f).err().unwrap();
        assert!(e.contains("already bound"), "{e}");
        let e = open(&root.join("src"), &j, &f).err().unwrap();
        assert!(e.contains("already bound"), "{e}");
        // A sibling is a different root and is fine.
        let other = dir("two-other");
        assert!(open(&other, &j, &f).is_ok());
    }

    #[test]
    fn a_dead_owners_bind_on_the_same_root_is_reclaimed_by_the_next_open() {
        let (root, j) = project("dead");
        let f = Fake::default();
        std::mem::forget(open(&root, &j, &f).unwrap());
        let g = BindGrant::open(
            &root,
            "antspawn-run",
            &Policy::default(),
            &j,
            Box::new(f.clone()),
            &|_| false,
        )
        .unwrap();
        assert_eq!(
            j.entries().len(),
            1,
            "the old entry is gone, the new one is on file"
        );
        g.revoke().unwrap();
        assert!(f.state.borrow().is_empty());
    }

    #[test]
    fn a_protected_path_made_after_open_is_reported() {
        let (root, j) = project("late");
        let f = Fake::default();
        let g = open(&root, &j, &f).unwrap();
        assert!(g.new_protected_paths().unwrap().is_empty());
        fs::create_dir_all(root.join("src/.claude")).unwrap();
        assert_eq!(g.new_protected_paths().unwrap(), ["src/.claude"]);
    }

    #[test]
    fn odd_principals_and_roots_are_refused() {
        let (root, j) = project("odd");
        let f = Fake::default();
        for who in ["", "a:b", "a b", "x\"y", "a(OI)", "dom\\u"] {
            let e = BindGrant::open(
                &root,
                who,
                &Policy::default(),
                &j,
                Box::new(f.clone()),
                &|_| true,
            );
            assert!(e.is_err(), "`{who}` must be refused");
        }
        assert!(open(&root.join("missing"), &j, &f).is_err());
        assert!(
            open(&root.join("CLAUDE.md"), &j, &f).is_err(),
            "a file is not a root"
        );
        assert!(f.log.borrow().is_empty(), "nothing was touched");
    }

    #[test]
    fn a_link_in_the_project_is_refused_before_any_acl_change() {
        let (root, j) = project("link");
        let out = dir("link-out");
        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("j"))
            .arg(&out)
            .output()
            .is_ok_and(|o| o.status.success());
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&out, root.join("j")).is_ok();
        assert!(made, "the control needs a junction or symlink");
        let f = Fake::default();
        let e = open(&root, &j, &f).err().unwrap();
        assert!(e.contains("reparse"), "{e}");
        assert!(f.log.borrow().is_empty());
    }

    /// The real `icacls` on a throwaway tree, with the built-in Guests group standing in for the
    /// account (it exists on every box and nothing here relies on it). Proves the rule text is
    /// accepted, that the grant and the exact-rights deny show up, and that removal reads back.
    #[cfg(windows)]
    #[test]
    fn real_icacls_grants_denies_and_removes_with_read_back() {
        let (root, j) = project("real");
        let g = BindGrant::open(
            &root,
            "Guests",
            &Policy::default(),
            &j,
            Box::new(Icacls),
            &|_| true,
        )
        .unwrap();
        let on_root = icacls(&[root.as_os_str()]).unwrap();
        assert!(on_root.contains("Guests:(OI)(CI)(M)"), "{on_root}");
        let on_git = icacls(&[root.join(".git").as_os_str()]).unwrap();
        let git_line = on_git
            .lines()
            .find(|l| l.contains("Guests:(DENY)") || l.contains("Guests:(OI)(CI)(DENY)"))
            .unwrap_or_else(|| panic!("no deny on .git: {on_git}"));
        assert!(
            git_line.contains("DENY") && !git_line.contains("SYNCHRONIZE"),
            "{git_line}"
        );
        let on_file = icacls(&[root.join("CLAUDE.md").as_os_str()]).unwrap();
        assert!(on_file.contains("DENY"), "{on_file}");
        g.revoke().unwrap();
        for p in [root.clone(), root.join(".git"), root.join("CLAUDE.md")] {
            let t = icacls(&[p.as_os_str()]).unwrap();
            assert!(!t.contains("Guests:"), "{}: {t}", p.display());
        }
        assert!(j.entries().is_empty());
    }
}
