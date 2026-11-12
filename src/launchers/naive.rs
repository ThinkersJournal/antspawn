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
//!   `isolation.*` capabilities);
//! * its session (for the session controls) uses the same fixed directory and `close` does not
//!   wipe it, so a file from one session is there for the next (fails `clean-slate-next-session`).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::launcher::{Environment, Exit, LaunchError, Launcher};
use crate::session::{ExecOutcome, ExecRequest, Session};
use crate::session_files::WorkFiles;
use crate::session_proto::FileInfo;
use crate::spec::JobSpec;
use crate::streams::{Streams, Tail, settle};

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
        Ok(Box::new(NaiveEnv {
            dir,
            out: Tail::new(),
            err: Tail::new(),
            readers: Vec::new(),
        }))
    }

    fn open_session(&self) -> Result<Box<dyn Session>, LaunchError> {
        let dir = Naive::work_dir();
        std::fs::create_dir_all(&dir)?;
        Ok(Box::new(NaiveSession { dir }))
    }

    // Runs as the invoking user, so "bound" is just the folder itself: nothing to grant, nothing
    // isolated (the known-bad control).
    fn open_session_in(&self, root: Option<&Path>) -> Result<Box<dyn Session>, LaunchError> {
        match root {
            None => self.open_session(),
            Some(r) => Ok(Box::new(NaiveSession {
                dir: r.to_path_buf(),
            })),
        }
    }
}

/// The unisolated session: many execs in the one fixed directory, and `close` leaves it as it is.
struct NaiveSession {
    dir: PathBuf,
}

impl Session for NaiveSession {
    fn exec(&mut self, req: &ExecRequest) -> Result<ExecOutcome, LaunchError> {
        req.validate().map_err(LaunchError::Other)?;
        let cwd = req
            .cwd
            .as_ref()
            .map_or_else(|| self.dir.clone(), |c| self.dir.join(c));
        let (out, err) = (Tail::new(), Tail::new());
        let mut readers = Vec::new();
        let exit = run_command(
            &req.cmd,
            &cwd,
            req.env.iter(),
            req.time_s.map(Duration::from_secs),
            (&out, &err),
            &mut readers,
        )?;
        settle(&mut readers, Duration::from_millis(500));
        Ok(ExecOutcome {
            exit,
            streams: Streams {
                stdout: Some(out.snapshot()),
                stderr: Some(err.snapshot()),
                combined: false,
            },
        })
    }

    fn close(self: Box<Self>) -> Result<(), LaunchError> {
        // Deliberately leaves `self.dir` as it is.
        Ok(())
    }

    // The unisolated launcher serves the file operations straight from its directory.
    fn put(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<u64, LaunchError> {
        self.files()
            .put(path, offset, data)
            .map_err(LaunchError::Other)
    }

    fn get(&mut self, path: &str, offset: u64, len: u64) -> Result<(Vec<u8>, bool), LaunchError> {
        self.files()
            .get(path, offset, len)
            .map_err(LaunchError::Other)
    }

    fn list(&mut self, after: Option<&str>) -> Result<(Vec<FileInfo>, bool), LaunchError> {
        self.files().list(after).map_err(LaunchError::Other)
    }
}

impl NaiveSession {
    fn files(&self) -> WorkFiles {
        WorkFiles::new(
            &self.dir,
            crate::workspace::Policy::default().max_file_bytes,
        )
    }
}

struct NaiveEnv {
    dir: PathBuf,
    out: Tail,
    err: Tail,
    readers: Vec<std::thread::JoinHandle<()>>,
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
        run_command(
            &spec.job.cmd,
            &cwd,
            spec.job.env.iter(),
            spec.limits.time_s.map(Duration::from_secs),
            (&self.out, &self.err),
            &mut self.readers,
        )
    }

    fn streams(&mut self) -> Streams {
        settle(&mut self.readers, Duration::from_millis(500));
        Streams {
            stdout: Some(self.out.snapshot()),
            stderr: Some(self.err.snapshot()),
            combined: false,
        }
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

/// Start `cmd` in `cwd`, capture both streams into `tails`, and wait for it (or its limit).
fn run_command<'a>(
    cmd: &[String],
    cwd: &Path,
    env: impl Iterator<Item = (&'a String, &'a String)>,
    limit: Option<Duration>,
    tails: (&Tail, &Tail),
    readers: &mut Vec<std::thread::JoinHandle<()>>,
) -> Result<Exit, LaunchError> {
    let mut child = Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(cwd)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Detached readers: a descendant that keeps a pipe open must not stall the run.
    if let Some(p) = child.stdout.take() {
        readers.push(tails.0.capture(p));
    }
    if let Some(p) = child.stderr.take() {
        readers.push(tails.1.capture(p));
    }
    wait(&mut child, limit)
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
