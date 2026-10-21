// SPDX-License-Identifier: MIT OR Apache-2.0
//! `naive`: a plain child process in a shared directory. It isolates **nothing**.
//!
//! It exists as the known-bad control: every negative test must FAIL this launcher, which proves
//! the tests can fail. The matrix never certifies it for any capability, so `spawn run` refuses it
//! for a spec that needs isolation. Deliberate defects, each one a control:
//! * the work dir is fixed and `destroy` does not wipe it (fails `clean-slate-next-run`);
//! * only the direct child is killed on timeout, never its descendants (fails
//!   `kills-runaway-process`);
//! * the job runs as the invoking user, so it reads and writes anywhere (fails both
//!   `isolation.*` capabilities).

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::launcher::{Environment, Exit, LaunchError, Launcher};
use crate::spec::JobSpec;

/// The unisolated launcher.
#[derive(Debug, Default)]
pub struct Naive;

impl Naive {
    /// The fixed directory every naive job runs in.
    pub fn work_dir() -> PathBuf {
        std::env::temp_dir().join("antspawn-naive")
    }
}

impl Launcher for Naive {
    fn id(&self) -> &str {
        "naive"
    }

    fn provision(&self, _spec: &JobSpec) -> Result<Box<dyn Environment>, LaunchError> {
        let dir = Naive::work_dir();
        std::fs::create_dir_all(&dir)?;
        Ok(Box::new(NaiveEnv { dir }))
    }
}

struct NaiveEnv {
    dir: PathBuf,
}

impl NaiveEnv {
    fn resolve(&self, guest: &str) -> PathBuf {
        self.dir.join(guest)
    }
}

impl Environment for NaiveEnv {
    fn run(&mut self, spec: &JobSpec) -> Result<Exit, LaunchError> {
        let cwd = match &spec.job.cwd {
            Some(c) => self.resolve(c),
            None => self.dir.clone(),
        };
        let mut cmd = Command::new(&spec.job.cmd[0]);
        cmd.args(&spec.job.cmd[1..])
            .current_dir(cwd)
            .envs(&spec.job.env);
        let mut child = cmd.spawn()?;
        wait(&mut child, spec.limits.time_s.map(Duration::from_secs))
    }

    fn collect(&mut self, spec: &JobSpec, results: &Path) -> Result<(), LaunchError> {
        std::fs::create_dir_all(results)?;
        for o in &spec.outputs {
            copy(&self.resolve(&o.guest), &results.join(&o.name))?;
        }
        Ok(())
    }

    fn destroy(self: Box<Self>) -> Result<(), LaunchError> {
        // Deliberately leaves `self.dir` as it is.
        Ok(())
    }
}

fn wait(child: &mut Child, limit: Option<Duration>) -> Result<Exit, LaunchError> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code().map_or(Exit::Killed, Exit::Code));
        }
        if limit.is_some_and(|l| start.elapsed() >= l) {
            // Only the direct child: descendants survive. That is the point of this launcher.
            child.kill()?;
            child.wait()?;
            return Ok(Exit::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn copy(from: &Path, to: &Path) -> Result<(), LaunchError> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(from, to)?;
    }
    Ok(())
}
