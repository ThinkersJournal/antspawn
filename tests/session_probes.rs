// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `concurrent` and `no-read-other-session` probes (slice A3 P3), proven able to fail and able
//! to pass without any account:
//!
//! * `naive` puts every session in one directory: it must FAIL `no-read-other-session` (the known-bad
//!   control: two sessions forced onto one identity read each other).
//! * `IsolatedFake` gives each session its own directory and refuses reads outside it: the probe must
//!   say "not violated". This shows the probe's logic, not any real isolation; the real pool is
//!   measured in `tests/account_pool.rs` (runner-only, UNVERIFIED until accounts exist).
//! * A pool of one (`OneAtATime`) must FAIL `concurrent`; two at a time must pass it.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use antspawn::launchers::Naive;
use antspawn::probes::Probes;
use antspawn::{ExecOutcome, ExecRequest, LaunchError, Launcher, Session};

const PROBE: &str = env!("CARGO_BIN_EXE_spawn-probe");

/// Runs commands in its own directory, and answers "no" (exit 10) to a `read` of any path outside it.
struct IsolatedFake {
    dir: PathBuf,
    live: Rc<Cell<usize>>,
}

impl Session for IsolatedFake {
    fn exec(&mut self, req: &ExecRequest) -> Result<ExecOutcome, LaunchError> {
        if let [_, op, path] = &req.cmd[..]
            && op == "read"
            && !PathBuf::from(path).starts_with(&self.dir)
        {
            return Ok(ExecOutcome {
                exit: antspawn::Exit::Code(10),
                streams: Default::default(),
            });
        }
        let mut r = req.clone();
        r.cwd = None;
        let mut inner = Naive.open_session_in(Some(&self.dir))?;
        let out = inner.exec(&r);
        let _ = inner.close();
        out
    }

    fn close(self: Box<Self>) -> Result<(), LaunchError> {
        self.live.set(self.live.get() - 1);
        Ok(())
    }
}

fn fake_opener(
    tag: &str,
    limit: usize,
) -> (impl Fn() -> Result<Box<dyn Session>, LaunchError>, PathBuf) {
    let root = std::env::temp_dir().join(format!("antspawn-sp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let live = Rc::new(Cell::new(0usize));
    let n = Rc::new(Cell::new(0usize));
    let r = root.clone();
    let open = move || -> Result<Box<dyn Session>, LaunchError> {
        if live.get() >= limit {
            return Err(LaunchError::Other("pool exhausted".into()));
        }
        n.set(n.get() + 1);
        let dir = r.join(format!("s{}", n.get()));
        std::fs::create_dir_all(&dir)?;
        live.set(live.get() + 1);
        Ok(Box::new(IsolatedFake {
            dir,
            live: live.clone(),
        }))
    };
    (open, root)
}

#[test]
fn naive_fails_no_read_other_session_and_passes_concurrent() {
    let p = Probes::new(PROBE);
    let _ = std::fs::remove_dir_all(Naive::work_dir());
    let open = || Naive.open_session();
    assert!(
        p.violates_no_read_other_session(&open),
        "two naive sessions share a directory, so one must read the other's file"
    );
    assert!(!p.violates_concurrent(&open), "naive allows two sessions");
    let _ = std::fs::remove_dir_all(Naive::work_dir());
}

#[test]
fn separate_directories_pass_no_read_other_session() {
    let p = Probes::new(PROBE);
    let (open, root) = fake_opener("iso", 2);
    assert!(!p.violates_no_read_other_session(&open));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_pool_of_one_fails_concurrent_and_a_pool_of_two_passes() {
    let p = Probes::new(PROBE);
    let (one, r1) = fake_opener("one", 1);
    assert!(p.violates_concurrent(&one), "the second open was refused");
    let (two, r2) = fake_opener("two", 2);
    assert!(!p.violates_concurrent(&two));
    for r in [r1, r2] {
        let _ = std::fs::remove_dir_all(r);
    }
}
