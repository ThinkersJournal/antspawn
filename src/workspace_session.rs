// SPDX-License-Identifier: MIT OR Apache-2.0
//! A session with a copy-back workspace (slice W5 part 2 of `docs/SESSION-PLAN.md`).
//!
//! The launcher cannot touch the sandbox account's work dir, so everything crosses the session's
//! `put` / `get` / `list` operations. On open the project is walked and every file `put`; the BASE
//! manifest is built here from the bytes actually sent, never from anything the host reports. On
//! [`WorkspaceSession::changes`] the host's `list` says what differs, the differing files are `get`
//! into a launcher-side staging directory and re-hashed locally, and the result is diffed against the
//! base. The host is untrusted: its paths are checked, its sizes are capped, and its hashes only pick
//! what to fetch. AntSpawn never approves anything: [`WorkspaceSession::apply`] takes the harness's
//! [`Approvals`].

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ring::digest;

use crate::launcher::LaunchError;
use crate::runner::safe_relative;
use crate::session::{ExecOutcome, ExecRequest, Session};
use crate::session_proto::{FileInfo, MAX_CHUNK};
use crate::workspace::{
    ApplyReport, Approvals, ChangeSet, Entry, Manifest, Mode, Policy, WorkspaceError, apply,
    hash_file, walk,
};

static STAGES: AtomicU64 = AtomicU64::new(0);

fn bad(e: impl std::fmt::Display) -> LaunchError {
    LaunchError::Other(e.to_string())
}

fn werr(e: WorkspaceError) -> LaunchError {
    LaunchError::Other(e.to_string())
}

/// A session plus the project it was seeded from.
pub struct WorkspaceSession {
    inner: Box<dyn Session>,
    root: PathBuf,
    policy: Policy,
    base: Manifest,
}

/// The files a session changed, fetched into a launcher-side directory that is removed on drop.
#[derive(Debug)]
pub struct Staged {
    pub changes: ChangeSet,
    dir: PathBuf,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl WorkspaceSession {
    /// Copy `root` into the session and keep the base manifest. Only [`Mode::CopyBack`] is served
    /// here; on any failure the session is closed before the error returns, so none is left behind.
    pub fn open(
        mut inner: Box<dyn Session>,
        root: &Path,
        mode: Mode,
        policy: Policy,
    ) -> Result<WorkspaceSession, LaunchError> {
        if mode != Mode::CopyBack {
            let _ = inner.close();
            return Err(LaunchError::Other(format!(
                "workspace mode `{}` is not available yet; only `copy-back` is",
                mode.as_str()
            )));
        }
        match send_project(inner.as_mut(), root, &policy) {
            Ok(base) => Ok(WorkspaceSession {
                inner,
                root: root.to_path_buf(),
                policy,
                base,
            }),
            Err(e) => {
                let _ = inner.close();
                Err(e)
            }
        }
    }

    /// What the project looked like when it went in, as hashed by this side.
    pub fn base(&self) -> &Manifest {
        &self.base
    }

    /// Fetch what the session changed into a staging directory and diff it against the base.
    pub fn changes(&mut self) -> Result<Staged, LaunchError> {
        let dir = std::env::temp_dir().join(format!(
            "antspawn-stage-{}-{}",
            std::process::id(),
            STAGES.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        // From here the guard removes the directory on any early return.
        let mut staged = Staged {
            changes: ChangeSet::default(),
            dir,
        };
        let mut listed = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let (page, more) = self.inner.list(after.as_deref())?;
            if more && page.is_empty() {
                return Err(bad("session host: a list page was empty but claimed more"));
            }
            after = page.last().map(|f| f.path.clone());
            listed.extend(page);
            if !more {
                break;
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut total = 0u64;
        for f in &listed {
            check_path(&f.path)?;
            if !seen.insert(f.path.as_str()) {
                return Err(bad(format!("session host listed `{}` twice", f.path)));
            }
            if self
                .base
                .get(&f.path)
                .is_some_and(|b| b.size == f.size && b.sha256 == f.sha256)
            {
                continue;
            }
            if f.size > self.policy.max_file_bytes {
                return Err(bad(format!(
                    "`{}` is {} bytes; the per-file cap is {}",
                    f.path, f.size, self.policy.max_file_bytes
                )));
            }
            total += f.size;
            if total > self.policy.max_total_bytes {
                return Err(bad(format!(
                    "the changed files exceed the {} byte cap",
                    self.policy.max_total_bytes
                )));
            }
            let to = staged.dir.join(&f.path);
            self.fetch(f, &to)?;
            // Authoritative: what actually arrived, not what the host said.
            let now = hash_file(&to).map_err(werr)?;
            match self.base.get(&f.path) {
                Some(b) if *b == now => {} // changed and changed back between list and get
                Some(_) => {
                    staged.changes.modified.insert(f.path.clone(), now);
                }
                None => {
                    staged.changes.added.insert(f.path.clone(), now);
                }
            }
        }
        for p in self.base.keys() {
            if !seen.contains(p.as_str()) {
                staged.changes.deleted.insert(p.clone());
            }
        }
        Ok(staged)
    }

    fn fetch(&mut self, f: &FileInfo, to: &Path) -> Result<(), LaunchError> {
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut out = fs::File::create(to)?;
        let mut offset = 0u64;
        loop {
            let (bytes, eof) = self.inner.get(&f.path, offset, MAX_CHUNK)?;
            offset += bytes.len() as u64;
            if offset > self.policy.max_file_bytes {
                return Err(bad(format!(
                    "`{}` grew past the {} byte per-file cap while it was read",
                    f.path, self.policy.max_file_bytes
                )));
            }
            std::io::Write::write_all(&mut out, &bytes)?;
            if eof {
                return Ok(());
            }
            if bytes.is_empty() {
                return Err(bad(format!(
                    "session host sent no data for `{}` but not the end",
                    f.path
                )));
            }
        }
    }

    /// Write `staged` into the real project, as the user. Refusals and conflicts come back in the
    /// report with nothing written; `ok` is the harness's approval, never this crate's.
    pub fn apply(&self, staged: &Staged, ok: &Approvals) -> Result<ApplyReport, LaunchError> {
        apply(
            &staged.changes,
            &self.base,
            &staged.dir,
            &self.root,
            &self.policy,
            ok,
        )
        .map_err(werr)
    }
}

impl Session for WorkspaceSession {
    fn exec(&mut self, req: &ExecRequest) -> Result<ExecOutcome, LaunchError> {
        self.inner.exec(req)
    }
    fn close(self: Box<Self>) -> Result<(), LaunchError> {
        self.inner.close()
    }
    fn put(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<u64, LaunchError> {
        self.inner.put(path, offset, data)
    }
    fn get(&mut self, path: &str, offset: u64, len: u64) -> Result<(Vec<u8>, bool), LaunchError> {
        self.inner.get(path, offset, len)
    }
    fn list(&mut self, after: Option<&str>) -> Result<(Vec<FileInfo>, bool), LaunchError> {
        self.inner.list(after)
    }
}

/// A path from the host that is safe to join under the staging directory.
fn check_path(p: &str) -> Result<(), LaunchError> {
    let plain = safe_relative(p)
        && !p.contains('\\')
        && !p.contains(':')
        && p.split('/').all(|c| !c.is_empty() && c != ".");
    if plain {
        Ok(())
    } else {
        Err(bad(format!("session host listed an unsafe path `{p}`")))
    }
}

/// `put` every file under `root`, hashing the bytes as they are read so the manifest describes
/// exactly what was sent.
fn send_project(
    s: &mut dyn Session,
    root: &Path,
    policy: &Policy,
) -> Result<Manifest, LaunchError> {
    let mut manifest = Manifest::new();
    let mut total = 0u64;
    let mut failed: Option<LaunchError> = None;
    let walked = walk(
        root,
        "",
        &mut |rel, abs| match send_file(s, rel, abs, policy, &mut total) {
            Ok(e) => {
                manifest.insert(rel.to_string(), e);
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                failed = Some(e);
                Err(WorkspaceError::Io(msg))
            }
        },
    );
    if let Some(e) = failed {
        return Err(e);
    }
    walked.map_err(werr)?;
    Ok(manifest)
}

fn send_file(
    s: &mut dyn Session,
    rel: &str,
    abs: &Path,
    policy: &Policy,
    total: &mut u64,
) -> Result<Entry, LaunchError> {
    let mut f = fs::File::open(abs)?;
    let mut ctx = digest::Context::new(&digest::SHA256);
    let mut buf = vec![0u8; MAX_CHUNK as usize];
    let mut offset = 0u64;
    loop {
        let mut n = 0;
        while n < buf.len() {
            let r = f.read(&mut buf[n..])?;
            if r == 0 {
                break;
            }
            n += r;
        }
        if n == 0 && offset > 0 {
            break;
        }
        offset += n as u64;
        *total += n as u64;
        if offset > policy.max_file_bytes {
            return Err(bad(format!(
                "`{rel}` exceeds the per-file cap of {} bytes",
                policy.max_file_bytes
            )));
        }
        if *total > policy.max_total_bytes {
            return Err(bad(format!(
                "the project exceeds the {} byte copy cap",
                policy.max_total_bytes
            )));
        }
        ctx.update(&buf[..n]);
        // Offset 0 with no data still creates the (empty) file.
        let at = offset - n as u64;
        let size = s.put(rel, at, &buf[..n])?;
        if size != offset {
            return Err(bad(format!(
                "session host stored {size} bytes of `{rel}`, expected {offset}"
            )));
        }
        if n < buf.len() {
            break;
        }
    }
    let sha256 = ctx
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(Entry {
        size: offset,
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_files::WorkFiles;

    /// A session whose workspace is a directory the test can also read, serving the file ops the way
    /// the host does.
    struct DirSession(WorkFiles);

    impl Session for DirSession {
        fn exec(&mut self, _: &ExecRequest) -> Result<ExecOutcome, LaunchError> {
            Err(bad("no exec"))
        }
        fn close(self: Box<Self>) -> Result<(), LaunchError> {
            Ok(())
        }
        fn put(&mut self, p: &str, o: u64, d: &[u8]) -> Result<u64, LaunchError> {
            self.0.put(p, o, d).map_err(LaunchError::Other)
        }
        fn get(&mut self, p: &str, o: u64, l: u64) -> Result<(Vec<u8>, bool), LaunchError> {
            self.0.get(p, o, l).map_err(LaunchError::Other)
        }
        fn list(&mut self, a: Option<&str>) -> Result<(Vec<FileInfo>, bool), LaunchError> {
            self.0.list(a).map_err(LaunchError::Other)
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antspawn-wss-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(root: &Path, rel: &str, body: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn open(tag: &str) -> (WorkspaceSession, PathBuf, PathBuf) {
        let real = dir(&format!("{tag}-real"));
        let work = dir(&format!("{tag}-work"));
        write(&real, "src/a.rs", b"fn a() {}");
        write(&real, "b.txt", b"bee");
        write(&real, "empty", b"");
        let s = Box::new(DirSession(WorkFiles::new(&work, 1 << 20)));
        let ws = WorkspaceSession::open(s, &real, Mode::CopyBack, Policy::default()).unwrap();
        (ws, real, work)
    }

    #[test]
    fn the_project_goes_in_and_the_base_is_what_was_sent() {
        let (ws, real, work) = open("in");
        assert_eq!(ws.base().len(), 3);
        for (p, e) in ws.base() {
            assert_eq!(hash_file(&work.join(p)).unwrap(), *e, "{p}");
            assert_eq!(hash_file(&real.join(p)).unwrap(), *e, "{p}");
        }
        assert!(work.join("empty").is_file(), "an empty file still goes in");
    }

    #[test]
    fn a_large_file_crosses_in_several_chunks_and_back() {
        let real = dir("big-real");
        let work = dir("big-work");
        let body: Vec<u8> = (0..(MAX_CHUNK * 2 + 7)).map(|i| (i % 251) as u8).collect();
        write(&real, "big.bin", &body);
        let s = Box::new(DirSession(WorkFiles::new(&work, 1 << 22)));
        let mut ws = WorkspaceSession::open(s, &real, Mode::CopyBack, Policy::default()).unwrap();
        assert_eq!(fs::read(work.join("big.bin")).unwrap(), body);
        write(&work, "big.bin", &[body.as_slice(), b"!"].concat());
        let st = ws.changes().unwrap();
        assert_eq!(st.changes.modified.len(), 1);
        assert_eq!(
            fs::read(st.dir.join("big.bin")).unwrap().len(),
            body.len() + 1
        );
    }

    #[test]
    fn an_untouched_session_has_no_changes_and_stages_nothing() {
        let (mut ws, _, _) = open("same");
        let st = ws.changes().unwrap();
        assert!(st.changes.is_empty(), "{:?}", st.changes);
        assert_eq!(fs::read_dir(&st.dir).unwrap().count(), 0);
    }

    #[test]
    fn edits_adds_and_deletes_come_back_and_apply_writes_them() {
        let (mut ws, real, work) = open("edit");
        write(&work, "b.txt", b"BEE");
        write(&work, "new/c.txt", b"c");
        fs::remove_file(work.join("src/a.rs")).unwrap();
        let st = ws.changes().unwrap();
        assert_eq!(st.changes.modified.keys().collect::<Vec<_>>(), ["b.txt"]);
        assert_eq!(st.changes.added.keys().collect::<Vec<_>>(), ["new/c.txt"]);
        assert_eq!(st.changes.deleted.iter().collect::<Vec<_>>(), ["src/a.rs"]);
        // Nothing reaches the real tree until apply, and a delete needs the harness's say-so.
        assert_eq!(fs::read(real.join("b.txt")).unwrap(), b"bee");
        let refused = ws.apply(&st, &Approvals::default()).unwrap();
        assert!(!refused.is_clean());
        assert_eq!(fs::read(real.join("b.txt")).unwrap(), b"bee");
        let ok = Approvals {
            deletes: ["src/a.rs".to_string()].into(),
            ..Approvals::default()
        };
        let done = ws.apply(&st, &ok).unwrap();
        assert!(done.is_clean(), "{done:?}");
        assert_eq!(fs::read(real.join("b.txt")).unwrap(), b"BEE");
        assert_eq!(fs::read(real.join("new/c.txt")).unwrap(), b"c");
        assert!(!real.join("src/a.rs").exists());
    }

    #[test]
    fn a_protected_path_is_refused_without_its_approval() {
        let (mut ws, real, work) = open("prot");
        write(&work, ".github/workflows/ci.yml", b"on: push");
        let st = ws.changes().unwrap();
        let r = ws.apply(&st, &Approvals::default()).unwrap();
        assert!(!r.is_clean());
        assert!(!real.join(".github").exists());
    }

    /// Hostile host: lists a path that climbs out of the staging directory.
    struct Liar(Vec<FileInfo>);
    impl Session for Liar {
        fn exec(&mut self, _: &ExecRequest) -> Result<ExecOutcome, LaunchError> {
            Err(bad("no exec"))
        }
        fn close(self: Box<Self>) -> Result<(), LaunchError> {
            Ok(())
        }
        fn put(&mut self, _: &str, o: u64, d: &[u8]) -> Result<u64, LaunchError> {
            Ok(o + d.len() as u64)
        }
        fn get(&mut self, _: &str, _: u64, _: u64) -> Result<(Vec<u8>, bool), LaunchError> {
            Ok((b"x".to_vec(), true))
        }
        fn list(&mut self, _: Option<&str>) -> Result<(Vec<FileInfo>, bool), LaunchError> {
            Ok((self.0.clone(), false))
        }
    }

    fn info(p: &str) -> FileInfo {
        FileInfo {
            path: p.into(),
            size: 1,
            sha256: "00".into(),
        }
    }

    #[test]
    fn an_unsafe_path_from_the_host_is_refused_and_nothing_is_written_outside() {
        let real = dir("liar-real");
        for p in ["../escape.txt", "a\\b", "C:/x", "/abs", "a//b"] {
            let mut ws = WorkspaceSession::open(
                Box::new(Liar(vec![info(p)])),
                &real,
                Mode::CopyBack,
                Policy::default(),
            )
            .unwrap();
            let e = ws.changes().unwrap_err().to_string();
            assert!(e.contains("unsafe path"), "{p}: {e}");
        }
        assert!(!std::env::temp_dir().join("escape.txt").exists());
    }

    #[test]
    fn a_host_that_lists_a_path_twice_or_lies_about_size_cannot_smuggle_a_file() {
        let real = dir("dup-real");
        let mut ws = WorkspaceSession::open(
            Box::new(Liar(vec![info("a"), info("a")])),
            &real,
            Mode::CopyBack,
            Policy::default(),
        )
        .unwrap();
        assert!(ws.changes().unwrap_err().to_string().contains("twice"));
        // Says it is huge: refused before any byte is fetched.
        let mut big = info("a");
        big.size = u64::MAX / 2;
        let mut ws = WorkspaceSession::open(
            Box::new(Liar(vec![big])),
            &real,
            Mode::CopyBack,
            Policy::default(),
        )
        .unwrap();
        assert!(ws.changes().unwrap_err().to_string().contains("cap"));
    }

    #[test]
    fn the_base_is_not_taken_from_the_host_even_when_it_claims_a_match() {
        // The host reports the file with the base hash but serves other bytes: the unchanged
        // shortcut trusts the list, so a lying host can only HIDE a change from itself, never
        // forge one into the real tree. Here the lie is the opposite: a new hash for old bytes.
        let real = dir("fake-real");
        write(&real, "f", b"x");
        let base = hash_file(&real.join("f")).unwrap();
        let mut ws = WorkspaceSession::open(
            Box::new(Liar(vec![FileInfo {
                path: "f".into(),
                size: 1,
                sha256: "ff".into(),
            }])),
            &real,
            Mode::CopyBack,
            Policy::default(),
        )
        .unwrap();
        // Liar serves "x" for every get, which is exactly the base: no change is reported.
        let st = ws.changes().unwrap();
        assert!(st.changes.is_empty(), "{:?} vs {base:?}", st.changes);
    }

    #[test]
    fn bind_is_refused_and_a_failed_open_closes_the_session() {
        struct Watch(std::rc::Rc<std::cell::Cell<bool>>);
        impl Session for Watch {
            fn exec(&mut self, _: &ExecRequest) -> Result<ExecOutcome, LaunchError> {
                Err(bad("no exec"))
            }
            fn close(self: Box<Self>) -> Result<(), LaunchError> {
                self.0.set(true);
                Ok(())
            }
        }
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let real = dir("bind-real");
        let e = WorkspaceSession::open(
            Box::new(Watch(closed.clone())),
            &real,
            Mode::Bind,
            Policy::default(),
        );
        assert!(e.is_err_and(|e| e.to_string().contains("not available")));
        assert!(closed.get());
        // A session without file operations fails the copy-in and is closed too.
        write(&real, "f", b"x");
        closed.set(false);
        let e = WorkspaceSession::open(
            Box::new(Watch(closed.clone())),
            &real,
            Mode::CopyBack,
            Policy::default(),
        );
        assert!(e.is_err_and(|e| e.to_string().contains("file operations")));
        assert!(closed.get());
    }

    #[test]
    fn a_reparse_point_in_the_project_stops_the_copy_in() {
        let real = dir("link-real");
        let outside = dir("link-out");
        write(&outside, "secret", b"s");
        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(real.join("j"))
            .arg(&outside)
            .output()
            .is_ok_and(|o| o.status.success());
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&outside, real.join("j")).is_ok();
        assert!(made, "the control needs a junction or symlink");
        let work = dir("link-work");
        let r = WorkspaceSession::open(
            Box::new(DirSession(WorkFiles::new(&work, 1 << 20))),
            &real,
            Mode::CopyBack,
            Policy::default(),
        );
        assert!(r.is_err_and(|e| e.to_string().contains("reparse")));
        assert!(!work.join("j").exists());
    }
}
