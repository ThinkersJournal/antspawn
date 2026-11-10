// SPDX-License-Identifier: MIT OR Apache-2.0
//! The real Windows Sandbox driver (slice SB2 of `docs/SANDBOX-BROKER.md`): [`SandboxDriver`] over the
//! `wsb` command line, behind a [`Wsb`] runner so everything that is not the actual `wsb.exe` is
//! tested as text and fake output.
//!
//! A profile is a name for a **fixed template**: the full `.wsb` configuration text lives in a file the
//! operator listed in the broker's configuration, is read when the sandbox starts, and is passed to
//! `wsb start -c`. Nothing a client sent reaches `wsb`: the profile name selects one of the operator's
//! files, and the id passed to `stop` is one `wsb` itself printed.
//!
//! `stop` is idempotent and **reads the result back**: it lists the running sandboxes first (an id
//! that is not there is already gone, which is success), stops it, and lists again; the sandbox is
//! confirmed gone only if the second list no longer shows it. A stop that `wsb` reported as done but
//! the list still shows is an error, so the broker keeps the slot held and retries.
//!
//! UNVERIFIED, never run against a real `wsb` here: the format of `wsb list` beyond one id per line (the
//! launcher's `running_sandboxes` makes the same assumption), what `wsb stop` does for an id that is
//! already gone (this code never relies on it), and how long a stop takes to show in the list.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::launchers::windows_sandbox::sandbox_id_in;
use crate::sandbox_broker::SandboxDriver;

/// Runs `wsb` with the given arguments and returns its standard output.
pub trait Wsb {
    /// # Errors
    /// `wsb` could not start or exited non-zero (the text carries what it said).
    fn run(&self, args: &[&str]) -> Result<String, String>;
}

/// The real `wsb.exe`.
pub struct WsbExe;

impl Wsb for WsbExe {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let out = std::process::Command::new(crate::launchers::windows_sandbox::wsb_exe())
            .args(args)
            .output()
            .map_err(|e| format!("cannot start wsb: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            Ok(stdout)
        } else {
            Err(format!(
                "wsb {} exited {}: {} {}",
                args.first().copied().unwrap_or(""),
                out.status,
                stdout.trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

/// Largest template accepted: a `.wsb` file is a few hundred bytes.
const MAX_TEMPLATE: u64 = 64 * 1024;

pub struct WsbDriver<W: Wsb> {
    wsb: W,
    /// profile name -> the absolute path of its fixed `.wsb` file.
    templates: BTreeMap<String, PathBuf>,
}

impl<W: Wsb> WsbDriver<W> {
    pub fn new(wsb: W, templates: BTreeMap<String, PathBuf>) -> Self {
        Self { wsb, templates }
    }

    /// The ids `wsb list` shows, any owner. Only lines shaped like an id count (hex and dashes).
    fn running(&self) -> Result<Vec<String>, String> {
        let text = self.wsb.run(&["list"])?;
        Ok(text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
            .map(str::to_string)
            .collect())
    }

    fn template(&self, profile: &str) -> Result<String, String> {
        let path = self
            .templates
            .get(profile)
            .ok_or("no template for that profile")?;
        let meta = std::fs::metadata(path).map_err(|e| format!("cannot read the template: {e}"))?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_TEMPLATE {
            return Err("the template is not a plain non-empty file of at most 64 KiB".into());
        }
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read the template: {e}"))?;
        if !text.trim_start().starts_with("<Configuration") {
            return Err("the template is not a Windows Sandbox configuration".into());
        }
        Ok(text)
    }
}

impl<W: Wsb> SandboxDriver for WsbDriver<W> {
    fn start(&mut self, profile: &str) -> Result<String, String> {
        let config = self.template(profile)?;
        let said = self.wsb.run(&["start", "-c", &config])?;
        sandbox_id_in(&said).ok_or_else(|| "wsb start gave no sandbox id".to_string())
    }

    fn stop(&mut self, sandbox: &str) -> Result<(), String> {
        // The id came from `wsb start`; the shape check is the backstop before it is a command argument.
        if !sandbox.starts_with(|c: char| c.is_ascii_hexdigit())
            || !sandbox.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        {
            return Err("not a sandbox id".into());
        }
        if !self.running()?.iter().any(|r| r == sandbox) {
            return Ok(());
        }
        self.wsb.run(&["stop", "--id", sandbox])?;
        if self.running()?.iter().any(|r| r == sandbox) {
            return Err("wsb stop returned but the sandbox is still listed".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const ID: &str = "0f3a9c2e-1b4d-4e5f-8a6b-7c8d9e0f1a2b";
    const TEMPLATE: &str =
        "<Configuration>\n  <Networking>Disable</Networking>\n</Configuration>\n";

    /// Fake `wsb`: records every call; `list` answers from `running`; `stop` removes unless it sticks.
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<String>>,
        running: RefCell<Vec<String>>,
        stop_sticks: bool,
        start_says: String,
    }

    impl Wsb for &Fake {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            self.calls.borrow_mut().push(args.join(" "));
            match args[0] {
                "list" => Ok(format!("{}\n", self.running.borrow().join("\n"))),
                "start" => {
                    self.running.borrow_mut().push(ID.to_string());
                    Ok(self.start_says.clone())
                }
                "stop" => {
                    if !self.stop_sticks {
                        self.running.borrow_mut().retain(|r| r != args[2]);
                    }
                    Ok(String::new())
                }
                _ => Err("unexpected".into()),
            }
        }
    }

    fn template_file(tag: &str, text: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("antspawn-wsbd-{tag}-{}.wsb", std::process::id()));
        std::fs::write(&p, text).unwrap();
        p
    }

    fn driver(f: &Fake, p: PathBuf) -> WsbDriver<&Fake> {
        WsbDriver::new(f, BTreeMap::from([("default".to_string(), p)]))
    }

    fn fake() -> Fake {
        Fake {
            start_says: format!("Successfully started\nId: {ID}\n"),
            ..Fake::default()
        }
    }

    #[test]
    fn start_passes_the_fixed_template_and_returns_the_id_wsb_printed() {
        let f = fake();
        let p = template_file("start", TEMPLATE);
        let mut d = driver(&f, p.clone());
        assert_eq!(d.start("default").unwrap(), ID);
        let calls = f.calls.borrow().clone();
        assert_eq!(calls, vec![format!("start -c {TEMPLATE}")]);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn an_unlisted_profile_or_a_bad_template_never_reaches_wsb() {
        let f = fake();
        let good = template_file("bad-good", TEMPLATE);
        let mut d = driver(&f, good.clone());
        assert!(d.start("other").is_err());
        let empty = template_file("bad-empty", "");
        assert!(driver(&f, empty.clone()).start("default").is_err());
        let notcfg = template_file("bad-notcfg", "<Not>a config</Not>");
        assert!(driver(&f, notcfg.clone()).start("default").is_err());
        let huge = template_file(
            "bad-huge",
            &format!("<Configuration>{}", "x".repeat(70_000)),
        );
        assert!(driver(&f, huge.clone()).start("default").is_err());
        assert!(
            driver(&f, PathBuf::from(r"C:\no\such\file.wsb"))
                .start("default")
                .is_err()
        );
        assert!(f.calls.borrow().is_empty(), "{:?}", f.calls.borrow());
        for p in [good, empty, notcfg, huge] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn a_start_that_prints_no_id_is_an_error() {
        let mut f = fake();
        f.start_says = "something odd\n".into();
        let p = template_file("noid", TEMPLATE);
        assert!(driver(&f, p.clone()).start("default").is_err());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn stop_lists_stops_and_reads_back_that_it_is_gone() {
        let f = fake();
        f.running.borrow_mut().push(ID.to_string());
        let p = template_file("stop", TEMPLATE);
        let mut d = driver(&f, p.clone());
        d.stop(ID).unwrap();
        assert_eq!(
            f.calls.borrow().clone(),
            vec![
                "list".to_string(),
                format!("stop --id {ID}"),
                "list".to_string()
            ]
        );
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn stopping_an_id_that_is_not_running_is_success_and_never_calls_stop() {
        let f = fake();
        let p = template_file("gone", TEMPLATE);
        driver(&f, p.clone()).stop(ID).unwrap();
        assert_eq!(f.calls.borrow().clone(), vec!["list".to_string()]);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_stop_that_does_not_make_it_disappear_is_an_error() {
        let mut f = fake();
        f.stop_sticks = true;
        f.running.borrow_mut().push(ID.to_string());
        let p = template_file("sticks", TEMPLATE);
        let e = driver(&f, p.clone()).stop(ID).unwrap_err();
        assert!(e.contains("still listed"), "{e}");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn only_another_owners_sandbox_is_never_stopped_and_odd_ids_are_refused() {
        let f = fake();
        let other = "aaaaaaaa-0000-0000-0000-000000000000";
        f.running.borrow_mut().push(other.to_string());
        let p = template_file("other", TEMPLATE);
        let mut d = driver(&f, p.clone());
        // not the id we were asked about: listed, found absent, nothing stopped
        d.stop(ID).unwrap();
        assert!(f.running.borrow().contains(&other.to_string()));
        for bad in ["", "x y", "--all", "id; calc", "..\\x"] {
            assert!(d.stop(bad).is_err(), "{bad:?}");
        }
        assert_eq!(f.calls.borrow().clone(), vec!["list".to_string()]);
        let _ = std::fs::remove_file(p);
    }
}
