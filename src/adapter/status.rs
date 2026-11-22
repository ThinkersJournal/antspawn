// SPDX-License-Identifier: MIT OR Apache-2.0
//! The adapter's status file and single-instance lock (`docs/ADAPTER-UNATTENDED.md`, option A, ruled by the PM 2026-11-21).
//!
//! The adapter writes `status.json` after every tick: when it last ticked, its pid, the groups it serves and whether a runner
//! is in flight. The PM's sweep reads the file's age and reports a stale tick; the adapter mails nobody. `scripts/adapter-run.ps1`
//! adds `last_exit_code` and `exited_unix` after the adapter ends. The file holds no secret: names, numbers and times only.
//!
//! One instance: the adapter keeps `<status>.lock` open with sharing denied, so a second start fails at once while the first lives,
//! and a dead process releases it (no stale lock to clean up).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// What the file says. `last_exit_code` and `exited_unix` are the wrapper's, absent while the adapter runs.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Status<'a> {
    pub tick_unix: u64,
    pub pid: u32,
    pub groups: Vec<&'a str>,
    /// A runner is running (the dispatcher takes one job at a time).
    pub jobs_in_flight: u32,
}

/// The text written for `s`.
pub fn render(s: &Status) -> String {
    let mut t = serde_json::to_string_pretty(s).unwrap_or_default();
    t.push('\n');
    t
}

/// Write `s` to `path` atomically (a temp file in the same folder, then a rename), so a reader never sees half a file.
pub fn write(path: &Path, s: &Status) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(render(s).as_bytes())
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The held lock. Dropping it (or the process dying) releases it.
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

impl Lock {
    /// Take `<status>.lock`. Fails if another process holds it.
    pub fn acquire(status: &Path) -> Result<Lock, String> {
        let mut p = status.as_os_str().to_owned();
        p.push(".lock");
        let p = PathBuf::from(p);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut o = OpenOptions::new();
        o.write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            o.share_mode(0); // no sharing: a second open fails while this one lives
        }
        let mut f = o.open(&p).map_err(|e| {
            format!(
                "another adapter holds {} (or it cannot be opened): {e}",
                p.display()
            )
        })?;
        #[cfg(not(windows))]
        {
            // no sharing mode off Windows: the lock is advisory there; the adapter's launchers are Windows-only anyway
            let _ = &mut f;
        }
        let _ = writeln!(f, "{}", std::process::id());
        Ok(Lock { _file: f })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antspawn-status-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_status_file_holds_names_and_numbers_and_is_replaced_whole() {
        let d = dir("w");
        let p = d.join("status.json");
        let s = Status {
            tick_unix: 1795303000,
            pid: 7,
            groups: vec!["antspawn-account"],
            jobs_in_flight: 1,
        };
        write(&p, &s).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["tick_unix"], 1795303000u64);
        assert_eq!(v["groups"][0], "antspawn-account");
        assert_eq!(v["jobs_in_flight"], 1);
        assert!(
            v.get("last_exit_code").is_none(),
            "the wrapper adds that after exit"
        );
        write(
            &p,
            &Status {
                tick_unix: 1795303020,
                ..s
            },
        )
        .unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("1795303020"));
        assert!(!d.join("status.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn a_second_instance_is_refused_while_the_first_lives_and_allowed_after() {
        let d = dir("l");
        let p = d.join("status.json");
        let first = Lock::acquire(&p).unwrap();
        let e = Lock::acquire(&p).unwrap_err();
        assert!(e.contains("another adapter"), "{e}");
        drop(first);
        assert!(Lock::acquire(&p).is_ok(), "a released lock is takeable");
        let _ = std::fs::remove_dir_all(&d);
    }
}
