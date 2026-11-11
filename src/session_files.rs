// SPDX-License-Identifier: MIT OR Apache-2.0
//! File access to a session's workspace for the host (slice W5 of `docs/SESSION-PLAN.md`).
//!
//! The session's work dir belongs to the sandbox account; the launcher's user can neither read nor
//! write it, by design. So a project goes in, and changes come out, as `put`, `get` and `list` frames
//! that the **host** serves against its own work dir. Everything here treats the peer, and whatever
//! the session's commands left on disk, as untrusted: a path must be relative and plain, a symlink or
//! junction is never followed (a command can make one), and a write never exceeds the file cap.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use crate::runner::safe_relative;
use crate::session_proto::{FileInfo, MAX_CHUNK, MAX_LIST};
use crate::workspace::{WorkspaceError, hash_file, is_reparse, walk};

/// The host's view of one session workspace.
pub struct WorkFiles {
    root: PathBuf,
    max_file_bytes: u64,
}

impl WorkFiles {
    pub fn new(root: impl Into<PathBuf>, max_file_bytes: u64) -> Self {
        WorkFiles {
            root: root.into(),
            max_file_bytes,
        }
    }

    /// `root/path`, refused unless the path is plain and no existing component is a reparse point.
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        if !safe_relative(path) || path.contains('\\') {
            return Err(format!(
                "path `{path}` must be relative, `/`-separated, with no `..` or drive"
            ));
        }
        let mut cur = self.root.clone();
        for c in path.split('/') {
            if c.is_empty() || c == "." {
                return Err(format!("path `{path}` has an empty or `.` component"));
            }
            cur.push(c);
            match fs::symlink_metadata(&cur) {
                Ok(md) if is_reparse(&md) => {
                    return Err(format!(
                        "`{path}` passes through a symlink or reparse point"
                    ));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{path}: {e}")),
            }
        }
        Ok(cur)
    }

    /// Write one chunk. Offset 0 creates or truncates; any other offset must equal the current length.
    /// Returns the length after the write.
    pub fn put(&self, path: &str, offset: u64, data: &[u8]) -> Result<u64, String> {
        if data.len() as u64 > MAX_CHUNK {
            return Err(format!("chunk of {} bytes exceeds {MAX_CHUNK}", data.len()));
        }
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|e| *e <= self.max_file_bytes)
            .ok_or_else(|| {
                format!(
                    "`{path}` would exceed the {} byte file cap",
                    self.max_file_bytes
                )
            })?;
        let to = self.resolve(path)?;
        if offset == 0 {
            if let Some(dir) = to.parent() {
                fs::create_dir_all(dir).map_err(|e| format!("{path}: {e}"))?;
            }
            // The parents were just made; check them again so a link made between is still refused.
            self.resolve(path)?;
            fs::write(&to, data).map_err(|e| format!("{path}: {e}"))?;
            return Ok(end);
        }
        let mut f = OpenOptions::new()
            .append(true)
            .open(&to)
            .map_err(|e| format!("{path}: {e}"))?;
        let len = f.metadata().map_err(|e| format!("{path}: {e}"))?.len();
        if len != offset {
            return Err(format!(
                "`{path}` is {len} bytes but the chunk starts at {offset}; a chunk was lost or repeated"
            ));
        }
        f.write_all(data).map_err(|e| format!("{path}: {e}"))?;
        Ok(end)
    }

    /// Read up to `len` bytes (capped at [`MAX_CHUNK`]) from `offset`; `eof` when it reached the end.
    pub fn get(&self, path: &str, offset: u64, len: u64) -> Result<(Vec<u8>, bool), String> {
        let from = self.resolve(path)?;
        let md = fs::symlink_metadata(&from).map_err(|e| format!("{path}: {e}"))?;
        if !md.is_file() {
            return Err(format!("`{path}` is not a regular file"));
        }
        let want = len.min(MAX_CHUNK);
        let mut f = fs::File::open(&from).map_err(|e| format!("{path}: {e}"))?;
        f.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("{path}: {e}"))?;
        let mut buf = Vec::new();
        f.take(want)
            .read_to_end(&mut buf)
            .map_err(|e| format!("{path}: {e}"))?;
        let eof = offset + buf.len() as u64 >= md.len();
        Ok((buf, eof))
    }

    /// Regular files sorted by path, after `after` (exclusive), at most [`MAX_LIST`]. A symlink or
    /// junction anywhere in the workspace is an error naming it, not a silent omission.
    pub fn list(&self, after: Option<&str>) -> Result<(Vec<FileInfo>, bool), String> {
        let mut paths = Vec::new();
        walk(&self.root, "", &mut |rel, _| {
            paths.push(rel.to_string());
            Ok(())
        })
        .map_err(|e| e.to_string())?;
        paths.sort();
        let mut rest = paths
            .into_iter()
            .filter(|p| after.is_none_or(|a| p.as_str() > a));
        let page: Vec<String> = rest.by_ref().take(MAX_LIST).collect();
        let more = rest.next().is_some();
        let files = page
            .into_iter()
            .map(|path| {
                let e =
                    hash_file(&self.root.join(&path)).map_err(|e: WorkspaceError| e.to_string())?;
                Ok(FileInfo {
                    path,
                    size: e.size,
                    sha256: e.sha256,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok((files, more))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(tag: &str) -> (WorkFiles, PathBuf) {
        let d = std::env::temp_dir().join(format!("antspawn-sf-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        (WorkFiles::new(&d, 1000), d)
    }

    #[test]
    fn a_file_put_in_chunks_reads_back_the_same_and_lists_with_its_hash() {
        let (w, d) = files("roundtrip");
        assert_eq!(w.put("a/b.txt", 0, b"hello ").unwrap(), 6);
        assert_eq!(w.put("a/b.txt", 6, b"world").unwrap(), 11);
        assert_eq!(fs::read(d.join("a/b.txt")).unwrap(), b"hello world");
        let (got, eof) = w.get("a/b.txt", 0, 5).unwrap();
        assert_eq!((got.as_slice(), eof), (&b"hello"[..], false));
        let (got, eof) = w.get("a/b.txt", 5, 100).unwrap();
        assert_eq!((got.as_slice(), eof), (&b" world"[..], true));
        let (list, more) = w.list(None).unwrap();
        assert!(!more);
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].path.as_str(), list[0].size), ("a/b.txt", 11));
        // sha256("hello world")
        assert_eq!(
            list[0].sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        // offset 0 again truncates; it is a new file, not an append
        w.put("a/b.txt", 0, b"x").unwrap();
        assert_eq!(fs::read(d.join("a/b.txt")).unwrap(), b"x");
    }

    #[test]
    fn a_lost_or_repeated_chunk_and_an_oversize_file_are_refused() {
        let (w, d) = files("chunks");
        w.put("f", 0, b"abc").unwrap();
        assert!(w.put("f", 5, b"zz").is_err(), "a gap");
        assert!(w.put("f", 1, b"zz").is_err(), "a repeat");
        assert_eq!(
            fs::read(d.join("f")).unwrap(),
            b"abc",
            "unchanged by the refusals"
        );
        assert!(
            w.put("big", 0, &vec![0u8; 1001]).is_err(),
            "over the 1000 byte cap"
        );
        assert!(
            w.put("f", 999, b"zz").is_err(),
            "crossing the cap on a later chunk"
        );
        assert!(w.put("g", 0, &vec![0u8; MAX_CHUNK as usize + 1]).is_err());
    }

    #[test]
    fn paths_that_leave_the_workspace_are_refused() {
        let (w, d) = files("paths");
        for bad in [
            "",
            "..",
            "../x",
            "a/../../x",
            "/abs",
            "C:/x",
            "a\\b",
            "a//b",
            "./a",
        ] {
            assert!(w.put(bad, 0, b"x").is_err(), "put {bad:?}");
            assert!(w.get(bad, 0, 1).is_err(), "get {bad:?}");
        }
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0, "nothing was created");
        assert!(w.put("ok", 0, b"x").is_ok(), "control: a plain path works");
    }

    #[test]
    fn a_link_a_command_made_is_never_followed_by_put_get_or_list() {
        let outside = std::env::temp_dir().join(format!("antspawn-sf-out-{}", std::process::id()));
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), "s").unwrap();
        let (w, d) = files("link");
        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(d.join("lnk"))
            .arg(&outside)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&outside, d.join("lnk")).is_ok();
        if !made {
            eprintln!("SKIPPED: cannot create a directory link on this box");
            return;
        }
        assert!(w.get("lnk/secret", 0, 10).is_err());
        assert!(w.put("lnk/new", 0, b"x").is_err());
        assert!(
            !outside.join("new").exists(),
            "nothing written through the link"
        );
        assert!(
            w.list(None).is_err(),
            "a link in the workspace is named, not skipped"
        );
    }

    #[test]
    fn list_pages_in_path_order_and_says_when_more_follows() {
        let (w, _d) = files("pages");
        for i in 0..(MAX_LIST + 3) {
            w.put(&format!("f{i:05}"), 0, b"x").unwrap();
        }
        let (first, more) = w.list(None).unwrap();
        assert_eq!(first.len(), MAX_LIST);
        assert!(more);
        let (second, more) = w.list(Some(&first.last().unwrap().path)).unwrap();
        assert_eq!(second.len(), 3);
        assert!(!more);
        assert!(first.last().unwrap().path < second[0].path);
    }
}
