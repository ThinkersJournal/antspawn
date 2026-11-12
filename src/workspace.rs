// SPDX-License-Identifier: MIT OR Apache-2.0
//! Workspace modes for sessions (agent-loop ask A2; slices W1-W3 of `docs/SESSION-PLAN.md`).
//!
//! One setting chooses how a session's edits reach the real project: [`Mode::CopyBack`] (default)
//! gives the session a working copy and later returns a reviewed [`ChangeSet`]; [`Mode::Bind`] grants
//! the account the real folder (slice W4, not here). This module is the pure file logic of copy-back,
//! run by the **launcher's user, never the sandbox account**:
//!
//! 1. [`copy_in`] copies the project into the working copy and returns the base [`Manifest`]
//!    (path, size, SHA-256). Reparse points and symlinks are refused, never followed.
//! 2. [`diff`] compares the working copy to the manifest: added, modified, deleted.
//! 3. [`check`] applies the [`Policy`]: protected paths, deletes and size caps are *refused* unless
//!    the harness passes a per-file [`Approvals`] entry naming them. AntSpawn never approves.
//! 4. [`apply`] writes the change set into the real tree **all or nothing**: every entry is checked
//!    against the real file's current hash first, and any conflict or refusal applies nothing.
//!    The real file is never overwritten when it no longer matches the manifest.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use ring::digest;

/// How a session's edits reach the real project. One value, no third option and no "auto".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// A working copy plus a reviewed change set (the default).
    #[default]
    CopyBack,
    /// An ACL grant on the real folder (slice W4).
    Bind,
}

impl Mode {
    /// `"copy-back"` / `"copy"` or `"bind"`; anything else is refused, there is no fallback.
    pub fn parse(s: &str) -> Result<Mode, String> {
        match s {
            "copy-back" | "copy" => Ok(Mode::CopyBack),
            "bind" => Ok(Mode::Bind),
            other => Err(format!(
                "workspace mode `{other}`: expected `copy-back` or `bind`"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::CopyBack => "copy-back",
            Mode::Bind => "bind",
        }
    }
}

/// One file's identity in a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub size: u64,
    /// Lowercase hex SHA-256.
    pub sha256: String,
}

/// Path (relative, `/`-separated) to entry. Directories are not recorded; an empty one is not tracked.
pub type Manifest = BTreeMap<String, Entry>;

/// What may be copied and what a change set may contain.
#[derive(Debug, Clone)]
pub struct Policy {
    /// A name with no `/` matches any path component (`.git` also guards a nested one); a name
    /// with `/` matches a path prefix. Case-insensitive (Windows).
    pub protected: Vec<String>,
    /// Paths that run on a later step outside the sandbox; listed in the change set so an approval
    /// names what it enables, never refused. Same matching as `protected`.
    pub later_step: Vec<String>,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for Policy {
    fn default() -> Self {
        let v = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect();
        Policy {
            // `.cargo/config.toml` can name a build runner (agent-loop SANDBOX 5.6).
            protected: v(&[
                ".git",
                ".github",
                ".claude",
                ".githooks",
                ".husky",
                "CLAUDE.md",
                ".cargo/config.toml",
            ]),
            later_step: v(&["build.rs", "Makefile", "justfile", "package.json"]),
            max_file_bytes: 64 * 1024 * 1024,
            max_total_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

fn matches_any(list: &[String], path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    list.iter().any(|n| {
        let n = n.to_ascii_lowercase();
        if n.contains('/') {
            p == n || p.starts_with(&format!("{n}/"))
        } else {
            p.split('/').any(|c| c == n)
        }
    })
}

impl Policy {
    pub fn is_protected(&self, path: &str) -> bool {
        matches_any(&self.protected, path)
    }
    pub fn is_later_step(&self, path: &str) -> bool {
        matches_any(&self.later_step, path)
    }
}

/// Why a copy, diff or apply stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum WorkspaceError {
    Io(String),
    /// A symlink or reparse point (junction); named, never followed.
    Reparse(String),
    TooBig(String),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceError::Io(m) => write!(f, "{m}"),
            WorkspaceError::Reparse(p) => write!(f, "`{p}` is a symlink or reparse point; refused"),
            WorkspaceError::TooBig(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

fn io(path: &Path, e: std::io::Error) -> WorkspaceError {
    WorkspaceError::Io(format!("{}: {e}", path.display()))
}

/// True for a symlink or, on Windows, any reparse point (junctions included).
pub(crate) fn is_reparse(md: &fs::Metadata) -> bool {
    if md.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if md.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

/// SHA-256 of a file and its size, streamed.
pub fn hash_file(path: &Path) -> Result<Entry, WorkspaceError> {
    let mut f = fs::File::open(path).map_err(|e| io(path, e))?;
    let mut ctx = digest::Context::new(&digest::SHA256);
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| io(path, e))?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
        size += n as u64;
    }
    let hex = ctx
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(Entry { size, sha256: hex })
}

/// Walk `root`, calling `f(relative path, absolute path)` for every regular file, in sorted order.
/// A reparse point anywhere is an error (not skipped: a skipped one hides what the copy lacks).
pub(crate) fn walk(
    root: &Path,
    rel: &str,
    f: &mut dyn FnMut(&str, &Path) -> Result<(), WorkspaceError>,
) -> Result<(), WorkspaceError> {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let mut names: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| io(&dir, e))?
        .map(|e| e.map(|e| e.file_name()).map_err(|e| io(&dir, e)))
        .collect::<Result<_, _>>()?;
    names.sort();
    for name in names {
        let name = name.to_string_lossy().into_owned();
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let abs = dir.join(&name);
        let md = fs::symlink_metadata(&abs).map_err(|e| io(&abs, e))?;
        if is_reparse(&md) {
            return Err(WorkspaceError::Reparse(child_rel));
        }
        if md.is_dir() {
            walk(root, &child_rel, f)?;
        } else if md.is_file() {
            f(&child_rel, &abs)?;
        }
        // Anything else (devices, sockets) is not a project file and is not tracked.
    }
    Ok(())
}

/// Copy `src` into the empty or absent `dst` and return the base manifest. Nothing under `src` is
/// changed. `dst` must be empty so the manifest describes exactly what the copy holds.
pub fn copy_in(src: &Path, dst: &Path, policy: &Policy) -> Result<Manifest, WorkspaceError> {
    fs::create_dir_all(dst).map_err(|e| io(dst, e))?;
    if fs::read_dir(dst).map_err(|e| io(dst, e))?.next().is_some() {
        return Err(WorkspaceError::Io(format!(
            "{}: the working copy must start empty",
            dst.display()
        )));
    }
    let mut manifest = Manifest::new();
    let mut total = 0u64;
    walk(src, "", &mut |rel, abs| {
        let entry = hash_file(abs)?;
        if entry.size > policy.max_file_bytes {
            return Err(WorkspaceError::TooBig(format!(
                "`{rel}` is {} bytes; the per-file cap is {}",
                entry.size, policy.max_file_bytes
            )));
        }
        total += entry.size;
        if total > policy.max_total_bytes {
            return Err(WorkspaceError::TooBig(format!(
                "the project exceeds the {} byte copy cap",
                policy.max_total_bytes
            )));
        }
        let to = dst.join(rel);
        if let Some(p) = to.parent() {
            fs::create_dir_all(p).map_err(|e| io(p, e))?;
        }
        fs::copy(abs, &to).map_err(|e| io(&to, e))?;
        manifest.insert(rel.to_string(), entry);
        Ok(())
    })?;
    Ok(manifest)
}

/// What the session changed relative to its base manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangeSet {
    pub added: BTreeMap<String, Entry>,
    pub modified: BTreeMap<String, Entry>,
    pub deleted: BTreeSet<String>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }

    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.added
            .keys()
            .chain(self.modified.keys())
            .chain(self.deleted.iter())
    }

    /// SHA-256 (hex) of a canonical listing: one `A`, `M` or `D` line per entry in path order,
    /// with size and content hash for adds and modifies. Two sets have one hash only if they name
    /// the same paths with the same content, so a reviewer's approval binds to what is applied.
    pub fn set_hash(&self) -> String {
        let mut ctx = digest::Context::new(&digest::SHA256);
        for (tag, map) in [("A", &self.added), ("M", &self.modified)] {
            for (p, e) in map {
                ctx.update(format!("{tag}\t{p}\t{}\t{}\n", e.size, e.sha256).as_bytes());
            }
        }
        for p in &self.deleted {
            ctx.update(format!("D\t{p}\n").as_bytes());
        }
        ctx.finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Entries that run on a later step outside the sandbox (build scripts, CI): shown so an
    /// approval names what it enables.
    pub fn later_step(&self, policy: &Policy) -> Vec<&String> {
        self.paths().filter(|p| policy.is_later_step(p)).collect()
    }
}

/// Compare the working copy to `base`. A reparse point in the copy is an error.
pub fn diff(base: &Manifest, work: &Path) -> Result<ChangeSet, WorkspaceError> {
    let mut cs = ChangeSet::default();
    let mut seen = BTreeSet::new();
    walk(work, "", &mut |rel, abs| {
        seen.insert(rel.to_string());
        match base.get(rel) {
            None => {
                cs.added.insert(rel.to_string(), hash_file(abs)?);
            }
            Some(b) => {
                // Size first: a differing size needs no hash to be a change.
                let now = hash_file(abs)?;
                if now != *b {
                    cs.modified.insert(rel.to_string(), now);
                }
            }
        }
        Ok(())
    })?;
    for p in base.keys() {
        if !seen.contains(p) {
            cs.deleted.insert(p.clone());
        }
    }
    Ok(cs)
}

/// What the harness approved, by exact path. AntSpawn holds no approval of its own.
#[derive(Debug, Clone, Default)]
pub struct Approvals {
    pub deletes: BTreeSet<String>,
    pub protected: BTreeSet<String>,
}

/// One refused entry and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub path: String,
    pub why: String,
}

/// Entries the policy refuses without an approval naming them.
pub fn check(cs: &ChangeSet, policy: &Policy, ok: &Approvals) -> Vec<Refusal> {
    let mut out = Vec::new();
    for p in cs.paths() {
        if policy.is_protected(p) && !ok.protected.contains(p) {
            out.push(Refusal {
                path: p.clone(),
                why: "protected path (a change to it alters what runs later outside the sandbox)"
                    .into(),
            });
        }
    }
    for p in &cs.deleted {
        if !ok.deletes.contains(p) && !policy.is_protected(p) {
            out.push(Refusal {
                path: p.clone(),
                why: "delete not approved for this file".into(),
            });
        }
    }
    for (p, e) in cs.added.iter().chain(cs.modified.iter()) {
        if e.size > policy.max_file_bytes {
            out.push(Refusal {
                path: p.clone(),
                why: format!(
                    "{} bytes exceeds the {} byte cap",
                    e.size, policy.max_file_bytes
                ),
            });
        }
    }
    out
}

/// One entry whose real file no longer matches what the session started from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub path: String,
    pub why: String,
}

/// The result of an [`apply`]. When `refused` or `conflicts` is non-empty nothing was written.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub applied: Vec<String>,
    pub refused: Vec<Refusal>,
    pub conflicts: Vec<Conflict>,
}

impl ApplyReport {
    pub fn is_clean(&self) -> bool {
        self.refused.is_empty() && self.conflicts.is_empty()
    }
}

/// True if `real/rel` or any existing ancestor inside `real` is a reparse point.
fn real_path_redirected(real: &Path, rel: &str) -> Result<Option<String>, WorkspaceError> {
    let mut cur = real.to_path_buf();
    let mut acc = String::new();
    for c in rel.split('/') {
        cur.push(c);
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(c);
        match fs::symlink_metadata(&cur) {
            Ok(md) if is_reparse(&md) => return Ok(Some(acc)),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(io(&cur, e)),
        }
    }
    Ok(None)
}

fn real_entry(real: &Path, rel: &str) -> Result<Option<Entry>, WorkspaceError> {
    let p = real.join(rel);
    match fs::symlink_metadata(&p) {
        Ok(md) if md.is_file() => hash_file(&p).map(Some),
        Ok(_) => Err(WorkspaceError::Io(format!(
            "{rel}: exists in the real tree and is not a regular file"
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(&p, e)),
    }
}

/// Write `cs` into `real`, taking file contents from `work`. Runs as the user, not the sandbox.
///
/// Policy refusals and conflicts are all found **before** the first write; if there are any, the
/// report names them and nothing is written. A conflict is: a modified or deleted file whose real
/// content no longer matches `base`, an added file that now exists in the real tree, or a path
/// whose real ancestors include a reparse point. The working copy is re-hashed at write time, so
/// a file changed after [`diff`] is a conflict too.
pub fn apply(
    cs: &ChangeSet,
    base: &Manifest,
    work: &Path,
    real: &Path,
    policy: &Policy,
    ok: &Approvals,
) -> Result<ApplyReport, WorkspaceError> {
    let mut report = ApplyReport {
        refused: check(cs, policy, ok),
        ..ApplyReport::default()
    };
    let mut conflict = |path: &str, why: String| {
        report.conflicts.push(Conflict {
            path: path.to_string(),
            why,
        })
    };
    for p in cs.paths() {
        if let Some(at) = real_path_redirected(real, p)? {
            conflict(
                p,
                format!("`{at}` in the real tree is a symlink or reparse point"),
            );
            continue;
        }
        let now = real_entry(real, p)?;
        if cs.added.contains_key(p) {
            if now.is_some() {
                conflict(
                    p,
                    "added in the session but now exists in the real tree".into(),
                );
            }
        } else {
            // modified or deleted: the real file must be what the session started from
            let expect = base.get(p);
            if now.as_ref() != expect {
                let what = if now.is_none() {
                    "no longer exists in the real tree"
                } else {
                    "changed in the real tree since the session started"
                };
                conflict(p, what.into());
            }
        }
        if let Some(want) = cs.added.get(p).or_else(|| cs.modified.get(p)) {
            let got = hash_file(&work.join(p))
                .map_err(|e| WorkspaceError::Io(format!("{p}: working copy unreadable: {e}")))?;
            if got != *want {
                conflict(
                    p,
                    "changed in the working copy after the change set was computed".into(),
                );
            }
        }
    }
    if !report.is_clean() {
        return Ok(report);
    }
    for (p, _) in cs.added.iter().chain(cs.modified.iter()) {
        let to = real.join(p);
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        }
        let mut tmp: PathBuf = to.clone();
        let name = format!("{}.antspawn-tmp", to.file_name().unwrap().to_string_lossy());
        tmp.set_file_name(name);
        fs::copy(work.join(p), &tmp).map_err(|e| io(&tmp, e))?;
        fs::rename(&tmp, &to).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            io(&to, e)
        })?;
        report.applied.push(p.clone());
    }
    for p in &cs.deleted {
        let to = real.join(p);
        fs::remove_file(&to).map_err(|e| io(&to, e))?;
        report.applied.push(p.clone());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antspawn-ws-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn get(root: &Path, rel: &str) -> Option<String> {
        fs::read_to_string(root.join(rel)).ok()
    }

    /// A real tree, a working copy of it and the base manifest.
    fn setup(tag: &str) -> (PathBuf, PathBuf, Manifest) {
        let real = dir(&format!("{tag}-real"));
        let work = dir(&format!("{tag}-work"));
        put(&real, "src/a.rs", "fn a() {}");
        put(&real, "src/b.rs", "fn b() {}");
        put(&real, "README.md", "hi");
        put(&real, ".git/config", "[core]");
        let m = copy_in(&real, &work, &Policy::default()).unwrap();
        (real, work, m)
    }

    #[test]
    fn mode_parses_two_values_and_refuses_the_rest() {
        assert_eq!(Mode::parse("copy-back"), Ok(Mode::CopyBack));
        assert_eq!(Mode::parse("copy"), Ok(Mode::CopyBack));
        assert_eq!(Mode::parse("bind"), Ok(Mode::Bind));
        for bad in ["", "auto", "Bind", "rw"] {
            assert!(Mode::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Mode::default(), Mode::CopyBack);
    }

    #[test]
    fn copy_in_records_every_file_and_leaves_the_source_alone() {
        let (real, work, m) = setup("copyin");
        assert_eq!(m.len(), 4);
        assert_eq!(get(&work, "src/a.rs").as_deref(), Some("fn a() {}"));
        assert_eq!(m["README.md"].size, 2);
        // sha256("hi")
        assert_eq!(
            m["README.md"].sha256,
            "8f434346648f6b96df89dda901c5176b10a6d83961dd3c1ac88b59b2dc327aa4"
        );
        assert_eq!(get(&real, "src/a.rs").as_deref(), Some("fn a() {}"));
        // a non-empty destination is refused: the manifest would not describe the copy
        assert!(copy_in(&real, &work, &Policy::default()).is_err());
    }

    #[test]
    fn diff_reports_edit_add_delete_and_not_the_untouched() {
        let (_real, work, m) = setup("diff");
        assert!(
            diff(&m, &work).unwrap().is_empty(),
            "control: no edit, no diff"
        );
        put(&work, "src/a.rs", "fn a() { 1 }");
        put(&work, "src/new.rs", "x");
        fs::remove_file(work.join("src/b.rs")).unwrap();
        let cs = diff(&m, &work).unwrap();
        assert_eq!(cs.modified.keys().collect::<Vec<_>>(), ["src/a.rs"]);
        assert_eq!(cs.added.keys().collect::<Vec<_>>(), ["src/new.rs"]);
        assert_eq!(cs.deleted.iter().collect::<Vec<_>>(), ["src/b.rs"]);
    }

    #[test]
    fn a_same_size_edit_is_still_a_change() {
        let (_real, work, m) = setup("samesize");
        put(&work, "README.md", "ho");
        assert!(diff(&m, &work).unwrap().modified.contains_key("README.md"));
    }

    #[test]
    fn apply_writes_the_edit_add_and_approved_delete() {
        let (real, work, m) = setup("apply");
        put(&work, "src/a.rs", "fn a() { 1 }");
        put(&work, "src/new/n.rs", "n");
        fs::remove_file(work.join("src/b.rs")).unwrap();
        let cs = diff(&m, &work).unwrap();
        let mut ok = Approvals::default();
        ok.deletes.insert("src/b.rs".into());
        let r = apply(&cs, &m, &work, &real, &Policy::default(), &ok).unwrap();
        assert!(r.is_clean(), "{r:?}");
        assert_eq!(r.applied.len(), 3);
        assert_eq!(get(&real, "src/a.rs").as_deref(), Some("fn a() { 1 }"));
        assert_eq!(get(&real, "src/new/n.rs").as_deref(), Some("n"));
        assert_eq!(get(&real, "src/b.rs"), None);
        assert_eq!(
            get(&real, "README.md").as_deref(),
            Some("hi"),
            "untouched stays"
        );
    }

    #[test]
    fn a_real_file_changed_since_the_session_began_is_a_conflict_and_nothing_is_written() {
        let (real, work, m) = setup("conflict");
        put(&work, "src/a.rs", "session edit");
        put(&work, "src/b.rs", "session edit b");
        // the user edits one of them in the real tree meanwhile
        put(&real, "src/a.rs", "user edit");
        let cs = diff(&m, &work).unwrap();
        let r = apply(
            &cs,
            &m,
            &work,
            &real,
            &Policy::default(),
            &Approvals::default(),
        )
        .unwrap();
        assert_eq!(r.conflicts.len(), 1, "{r:?}");
        assert_eq!(r.conflicts[0].path, "src/a.rs");
        assert!(r.applied.is_empty());
        assert_eq!(
            get(&real, "src/a.rs").as_deref(),
            Some("user edit"),
            "not overwritten"
        );
        assert_eq!(
            get(&real, "src/b.rs").as_deref(),
            Some("fn b() {}"),
            "all or nothing"
        );
    }

    #[test]
    fn an_added_file_that_now_exists_and_a_vanished_one_are_conflicts() {
        let (real, work, m) = setup("conflict2");
        put(&work, "new.txt", "session");
        put(&work, "src/a.rs", "session edit");
        put(&real, "new.txt", "user made it first");
        fs::remove_file(real.join("src/a.rs")).unwrap();
        let cs = diff(&m, &work).unwrap();
        let r = apply(
            &cs,
            &m,
            &work,
            &real,
            &Policy::default(),
            &Approvals::default(),
        )
        .unwrap();
        let paths: Vec<_> = r.conflicts.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["new.txt", "src/a.rs"], "{r:?}");
        assert_eq!(get(&real, "new.txt").as_deref(), Some("user made it first"));
    }

    #[test]
    fn protected_paths_are_refused_unless_approved_by_name() {
        let (real, work, m) = setup("protected");
        put(&work, ".git/config", "[core] evil");
        put(&work, ".github/workflows/x.yml", "on: push");
        put(&work, "sub/.git/hooks/pre-commit", "#!/bin/sh");
        put(&work, "CLAUDE.md", "obey");
        put(&work, "src/ok.rs", "fine");
        let cs = diff(&m, &work).unwrap();
        let r = apply(
            &cs,
            &m,
            &work,
            &real,
            &Policy::default(),
            &Approvals::default(),
        )
        .unwrap();
        let mut refused: Vec<_> = r.refused.iter().map(|x| x.path.as_str()).collect();
        refused.sort();
        assert_eq!(
            refused,
            [
                ".git/config",
                ".github/workflows/x.yml",
                "CLAUDE.md",
                "sub/.git/hooks/pre-commit"
            ]
        );
        assert!(r.applied.is_empty());
        assert_eq!(
            get(&real, "src/ok.rs"),
            None,
            "a refusal stops the whole apply"
        );
        assert_eq!(get(&real, ".git/config").as_deref(), Some("[core]"));
        // approving one by name lets exactly that one through; the others stay refused
        let mut ok = Approvals::default();
        ok.protected.insert("CLAUDE.md".into());
        let r = apply(&cs, &m, &work, &real, &Policy::default(), &ok).unwrap();
        assert_eq!(r.refused.len(), 3);
    }

    #[test]
    fn protected_matching_is_case_insensitive_and_does_not_overmatch() {
        let p = Policy::default();
        assert!(p.is_protected(".GIT/config"));
        assert!(p.is_protected("claude.md"));
        assert!(p.is_protected(".cargo/config.toml"));
        assert!(!p.is_protected(".cargo/other.toml"));
        assert!(!p.is_protected("src/.gitignore"), "a different name");
        assert!(!p.is_protected("docs/CLAUDE.md.bak"));
        assert!(!p.is_protected("src/main.rs"));
    }

    #[test]
    fn an_unapproved_delete_is_refused_and_an_approved_one_is_not() {
        let (real, work, m) = setup("delete");
        fs::remove_file(work.join("README.md")).unwrap();
        let cs = diff(&m, &work).unwrap();
        let r = apply(
            &cs,
            &m,
            &work,
            &real,
            &Policy::default(),
            &Approvals::default(),
        )
        .unwrap();
        assert_eq!(r.refused.len(), 1);
        assert_eq!(get(&real, "README.md").as_deref(), Some("hi"));
        let mut ok = Approvals::default();
        ok.deletes.insert("src/a.rs".into()); // a different file: no help
        assert_eq!(check(&cs, &Policy::default(), &ok).len(), 1);
        ok.deletes.insert("README.md".into());
        assert!(check(&cs, &Policy::default(), &ok).is_empty());
    }

    #[test]
    fn build_scripts_are_listed_for_the_approval_not_refused() {
        let (_real, work, m) = setup("later");
        put(&work, "build.rs", "fn main() {}");
        put(&work, "pkg/package.json", "{}");
        put(&work, "src/x.rs", "x");
        let cs = diff(&m, &work).unwrap();
        let p = Policy::default();
        assert!(check(&cs, &p, &Approvals::default()).is_empty());
        assert_eq!(
            cs.later_step(&p),
            [&"build.rs".to_string(), &"pkg/package.json".to_string()]
        );
    }

    #[test]
    fn the_size_caps_refuse_a_big_file_at_copy_in_and_in_a_change_set() {
        let real = dir("caps-real");
        put(&real, "big", "0123456789");
        let p = Policy {
            max_file_bytes: 5,
            ..Policy::default()
        };
        let err = copy_in(&real, &dir("caps-work"), &p).unwrap_err();
        assert!(matches!(err, WorkspaceError::TooBig(_)), "{err}");
        let p = Policy {
            max_total_bytes: 5,
            ..Policy::default()
        };
        assert!(copy_in(&real, &dir("caps-work2"), &p).is_err());
        // a change set carrying an oversize file
        let cs = ChangeSet {
            added: BTreeMap::from([(
                "x".to_string(),
                Entry {
                    size: 99,
                    sha256: String::new(),
                },
            )]),
            ..ChangeSet::default()
        };
        let p = Policy {
            max_file_bytes: 5,
            ..Policy::default()
        };
        assert_eq!(check(&cs, &p, &Approvals::default()).len(), 1);
    }

    #[test]
    fn a_working_copy_file_changed_after_the_diff_is_a_conflict() {
        let (real, work, m) = setup("late");
        put(&work, "src/a.rs", "first");
        let cs = diff(&m, &work).unwrap();
        put(&work, "src/a.rs", "swapped after review");
        let r = apply(
            &cs,
            &m,
            &work,
            &real,
            &Policy::default(),
            &Approvals::default(),
        )
        .unwrap();
        assert_eq!(r.conflicts.len(), 1, "{r:?}");
        assert_eq!(get(&real, "src/a.rs").as_deref(), Some("fn a() {}"));
    }

    /// A directory link is made with a junction on Windows and a symlink elsewhere; skipped where
    /// the box will not make one (no privilege), and the skip is printed, not silent.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    #[test]
    fn a_junction_or_symlink_in_the_source_or_working_copy_is_refused_not_followed() {
        let outside = dir("link-outside");
        put(&outside, "secret.txt", "s");
        let real = dir("link-real");
        put(&real, "a.txt", "a");
        if !link_dir(&outside, &real.join("lnk")) {
            eprintln!("SKIPPED: cannot create a directory link on this box");
            return;
        }
        let err = copy_in(&real, &dir("link-work"), &Policy::default()).unwrap_err();
        assert_eq!(err, WorkspaceError::Reparse("lnk".into()));
        // and one the session planted in its working copy
        let real2 = dir("link-real2");
        put(&real2, "a.txt", "a");
        let work = dir("link-work2");
        let m = copy_in(&real2, &work, &Policy::default()).unwrap();
        assert!(link_dir(&outside, &work.join("planted")));
        assert!(matches!(diff(&m, &work), Err(WorkspaceError::Reparse(_))));
    }

    #[test]
    fn a_real_tree_ancestor_that_became_a_link_is_a_conflict() {
        let outside = dir("redir-outside");
        let real = dir("redir-real");
        put(&real, "a.txt", "a");
        let work = dir("redir-work");
        let m = copy_in(&real, &work, &Policy::default()).unwrap();
        put(&work, "d/new.txt", "n");
        let cs = diff(&m, &work).unwrap();
        if !link_dir(&outside, &real.join("d")) {
            eprintln!("SKIPPED: cannot create a directory link on this box");
            return;
        }
        let r = apply(
            &cs,
            &m,
            &work,
            &real,
            &Policy::default(),
            &Approvals::default(),
        )
        .unwrap();
        assert_eq!(r.conflicts.len(), 1, "{r:?}");
        assert!(
            !outside.join("new.txt").exists(),
            "nothing written through the link"
        );
    }
}
