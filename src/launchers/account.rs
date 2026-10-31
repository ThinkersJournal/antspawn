// SPDX-License-Identifier: MIT OR Apache-2.0
//! `account`: run the job as a restricted Windows account that nobody holds the password of.
//!
//! The account's password lives only in the service manager. A one-time elevated setup (the
//! machine owner runs `scripts/account-setup.ps1 -Apply`) creates the account, the task
//! [`TASK_NAME`] that runs `spawn-runner serve` as it, and the spool/results directories. This
//! launcher never elevates, never creates an account, and never repairs a missing piece: it says
//! what is missing and stops.
//!
//! The launcher only copies files and triggers the task (see [`crate::runner`] for the layout):
//! * provision: new job id; refuse specs the runner would refuse; copy `ro` sources into
//!   `spool/<id>/in/<n>`; write `spool/<id>/spec.toml`;
//! * run: `schtasks /Run`, then poll `results/<id>/exit.code`, which the runner writes last,
//!   after the job's whole process tree is dead and its work dir is wiped;
//! * collect: copy `results/<id>/out/<name>` to the host results folder;
//! * destroy (and `Drop`): remove the job's spool and results directories.
//!
//! One job runs at a time (`concurrent` is never claimed). Capabilities are certified only by the
//! runner-only negative tests on a machine where the setup has been applied (UNVERIFIED until then).

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::launcher::{Environment, Exit, LaunchError, Launcher};
use crate::runner::{Layout, RunnerConfig, TASK_NAME, check_spec, decode_exit};
use crate::spec::{JobSpec, MountMode};
use crate::streams::Streams;

/// How long the runner gets to pick a job up and answer, beyond the job's own time limit.
const GRACE: Duration = Duration::from_secs(60);

/// The restricted-account launcher.
#[derive(Debug, Clone)]
pub struct Account {
    layout: Layout,
}

impl Account {
    pub fn new(layout: Layout) -> Self {
        Account { layout }
    }
}

impl Default for Account {
    fn default() -> Self {
        Account::new(Layout::from_env())
    }
}

impl Account {
    fn missing(&self, what: String) -> LaunchError {
        LaunchError::NotProvisioned {
            launcher: self.id().into(),
            what: format!(
                "{what}. Ask the machine owner to run `scripts/account-setup.ps1 -Apply` \
                 (one-time, elevated); this launcher never creates accounts or elevates."
            ),
        }
    }
}

impl Launcher for Account {
    fn id(&self) -> &str {
        "account"
    }

    fn provision(&self, spec: &JobSpec) -> Result<Box<dyn Environment>, LaunchError> {
        if !cfg!(windows) {
            return Err(self.missing("the account launcher is Windows-only".into()));
        }
        let cfg_text = std::fs::read_to_string(self.layout.config_file()).map_err(|e| {
            self.missing(format!(
                "cannot read {}: {e}",
                self.layout.config_file().display()
            ))
        })?;
        let cfg = RunnerConfig::from_toml(&cfg_text).map_err(LaunchError::Other)?;
        for d in [self.layout.spool(), self.layout.results()] {
            if !d.is_dir() {
                return Err(self.missing(format!("{} does not exist", d.display())));
            }
        }
        check_spec(spec, &cfg).map_err(LaunchError::Other)?;

        let id = new_job_id();
        let env = AccountEnv {
            layout: self.layout.clone(),
            id: id.clone(),
            limit: Duration::from_secs(cfg.time_limit_s(spec)),
        };
        // From here, dropping `env` removes whatever was created.
        let spool = self.layout.job_spool(&id);
        std::fs::create_dir_all(spool.join("in"))?;
        for (i, m) in spec.mounts.iter().enumerate() {
            if let (MountMode::Ro, Some(host)) = (m.mode, &m.host) {
                copy(host, &spool.join("in").join(i.to_string()))?;
            }
        }
        let text = toml::to_string(spec).map_err(|e| LaunchError::Other(e.to_string()))?;
        std::fs::write(spool.join("spec.toml"), text)?;
        Ok(Box::new(env))
    }
}

struct AccountEnv {
    layout: Layout,
    id: String,
    limit: Duration,
}

impl Environment for AccountEnv {
    fn run(&mut self, _spec: &JobSpec) -> Result<Exit, LaunchError> {
        let out = Command::new("schtasks")
            .args(["/Run", "/TN", TASK_NAME])
            .output()?;
        if !out.status.success() {
            return Err(LaunchError::NotProvisioned {
                launcher: "account".into(),
                what: format!(
                    "`schtasks /Run /TN {TASK_NAME}` failed ({}): {}. The task is missing or this \
                     user may not run it; the machine owner re-runs `scripts/account-setup.ps1 -Apply`.",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            });
        }
        let code_file = self.layout.job_results(&self.id).join("exit.code");
        let start = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(&code_file) {
                return decode_exit(&text).ok_or_else(|| {
                    LaunchError::Other(format!("runner wrote an unreadable exit.code: {text:?}"))
                });
            }
            if start.elapsed() > self.limit + GRACE {
                return Err(LaunchError::Other(format!(
                    "the runner did not answer within {}s; is task {TASK_NAME} registered and enabled?",
                    (self.limit + GRACE).as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn streams(&mut self) -> Streams {
        let dir = self.layout.job_results(&self.id).join("streams");
        let read = |n: &str| std::fs::read(dir.join(n)).ok();
        Streams {
            stdout: read("stdout.tail"),
            stderr: read("stderr.tail"),
            combined: false,
        }
    }

    fn collect(&mut self, spec: &JobSpec, results: &Path) -> Result<(), LaunchError> {
        std::fs::create_dir_all(results)?;
        let out = self.layout.job_results(&self.id).join("out");
        for o in &spec.outputs {
            let from = out.join(&o.name);
            if from.exists() {
                copy(&from, &results.join(&o.name))?;
            }
        }
        Ok(())
    }

    fn destroy(self: Box<Self>) -> Result<(), LaunchError> {
        self.remove()
    }
}

impl AccountEnv {
    fn remove(&self) -> Result<(), LaunchError> {
        let mut first: Option<std::io::Error> = None;
        for d in [
            self.layout.job_spool(&self.id),
            self.layout.job_results(&self.id),
        ] {
            match std::fs::remove_dir_all(&d) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => first = first.or(Some(e)),
            }
        }
        first.map_or(Ok(()), |e| Err(e.into()))
    }
}

impl Drop for AccountEnv {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

fn new_job_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{}-{nanos:x}", std::process::id())
}

fn copy(from: &Path, to: &Path) -> Result<(), LaunchError> {
    crate::runner::copy_tree(from, to).map_err(Into::into)
}
