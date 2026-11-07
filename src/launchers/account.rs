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
//! A **session** (slice S3 of `docs/SESSION-PLAN.md`) is the same two directories plus a named pipe:
//! the launcher writes `spool/<sid>/session.toml` (its own SID and a nonce), triggers the task, and
//! connects to `\\.\pipe\antspawn-session-<sid>-<nonce>`, whose DACL admits only that SID. `spawn-runner
//! session` hosts it in a kill-on-close Job Object; `exec` and `close` travel as [`crate::session_proto`]
//! frames. `close` (or dropping the session, which closes the pipe) ends the host.
//!
//! One job runs at a time (`concurrent` is never claimed). Capabilities are certified only by the
//! runner-only negative tests on a machine where the setup has been applied (UNVERIFIED until then).

use std::fs::File;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::launcher::{Environment, Exit, LaunchError, Launcher};
use crate::runner::{
    Layout, RunnerConfig, TASK_NAME, check_spec, decode_exit, reap_ended_sessions,
};
use crate::session::{ExecOutcome, ExecRequest, Session};
use crate::session_host::SessionFile;
use crate::session_proto::{Request, Response, read_frame, write_frame};
use crate::spec::{JobSpec, MountMode};
use crate::streams::Streams;

/// How long the runner gets to pick a job up and answer, beyond the job's own time limit.
const GRACE: Duration = Duration::from_secs(60);
/// How long an ended session may sit uncollected before the next `open_session` reaps it.
const ORPHAN_AGE: Duration = Duration::from_secs(120);

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

impl Account {
    /// The runner's ceilings, after checking that the one-time setup is in place.
    fn config(&self) -> Result<RunnerConfig, LaunchError> {
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
        Ok(cfg)
    }
}

impl Launcher for Account {
    fn id(&self) -> &str {
        "account"
    }

    fn provision(&self, spec: &JobSpec) -> Result<Box<dyn Environment>, LaunchError> {
        let cfg = self.config()?;
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

    fn open_session(&self) -> Result<Box<dyn Session>, LaunchError> {
        self.config()?;
        // Sessions whose owner died before it could collect them: ended long enough ago that no
        // live owner is still reading the entry. Best effort; a stuck one is reported by its absence
        // from the next call's list, never by failing this open.
        let _ = reap_ended_sessions(&self.layout, ORPHAN_AGE);
        let id = new_job_id();
        let file = SessionFile {
            launcher_sid: current_sid()?,
            nonce: new_nonce()?,
        };
        file.validate().map_err(LaunchError::Other)?;
        // From here, dropping `session` removes whatever was created.
        let mut session = AccountSession {
            layout: self.layout.clone(),
            id: id.clone(),
            pipe: None,
            next_id: 1,
        };
        let spool = self.layout.job_spool(&id);
        std::fs::create_dir_all(&spool)?;
        let text = toml::to_string(&file).map_err(|e| LaunchError::Other(e.to_string()))?;
        std::fs::write(spool.join("session.toml"), text)?;
        start_task()?;
        session.pipe = Some(connect(&file.pipe_name(&id))?);
        Ok(Box::new(session))
    }
}

/// Trigger the runner task (it runs `spawn-runner serve`, which hosts any pending session).
fn start_task() -> Result<(), LaunchError> {
    let out = Command::new("schtasks")
        .args(["/Run", "/TN", TASK_NAME])
        .output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(LaunchError::NotProvisioned {
        launcher: "account".into(),
        what: format!(
            "`schtasks /Run /TN {TASK_NAME}` failed ({}): {}. The task is missing or this \
             user may not run it; the machine owner re-runs `scripts/account-setup.ps1 -Apply`.",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    })
}

/// The SID of the user running this process, from `whoami /user`.
fn current_sid() -> Result<String, LaunchError> {
    let out = Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let sid = text
        .trim()
        .rsplit(',')
        .next()
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned();
    if out.status.success() && sid.starts_with("S-1-") {
        Ok(sid)
    } else {
        Err(LaunchError::Other(format!(
            "cannot read the launcher's SID from `whoami /user` ({})",
            out.status
        )))
    }
}

/// 128 random bits as hex: the unguessable part of the pipe name.
fn new_nonce() -> Result<String, LaunchError> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| LaunchError::Other("no secure random bytes for the pipe nonce".into()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Open the host's pipe. It does not exist until the runner task has started the host, so "not found"
/// and "all instances busy" are retried for [`GRACE`]; anything else (notably access denied, which
/// means the DACL does not admit this user) is final.
fn connect(name: &str) -> Result<File, LaunchError> {
    let path = format!(r"\\.\pipe\{name}");
    let start = Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(f) => return Ok(f),
            Err(e) if matches!(e.raw_os_error(), Some(2 | 231)) && start.elapsed() < GRACE => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(LaunchError::Other(format!(
                    "cannot reach the session host's pipe ({e}) after {}s; the runner runs one job \
                     or session at a time, so another may be holding it",
                    start.elapsed().as_secs()
                )));
            }
        }
    }
}

/// A live session on the restricted account: one pipe to the host that owns its Job Object.
struct AccountSession {
    layout: Layout,
    id: String,
    pipe: Option<File>,
    next_id: u64,
}

impl Session for AccountSession {
    fn exec(&mut self, req: &ExecRequest) -> Result<ExecOutcome, LaunchError> {
        req.validate().map_err(LaunchError::Other)?;
        let id = self.next_id;
        self.next_id += 1;
        let pipe = self
            .pipe
            .as_mut()
            .ok_or_else(|| LaunchError::Other("the session is closed".into()))?;
        let broken =
            |e: crate::session_proto::ProtoError| LaunchError::Other(format!("session host: {e}"));
        write_frame(pipe, &Request::exec(id, req)).map_err(broken)?;
        match read_frame::<_, Response>(pipe).map_err(broken)? {
            Some(Response::Error { message, .. }) => Err(LaunchError::Other(format!(
                "session host refused: {message}"
            ))),
            Some(Response::Closed {}) => Err(LaunchError::Other("the session host closed".into())),
            Some(r) => {
                let (got, outcome) = r.into_outcome().map_err(broken)?;
                if got == id {
                    Ok(outcome)
                } else {
                    Err(LaunchError::Other(format!(
                        "session host answered exec {got}, expected {id}"
                    )))
                }
            }
            None => Err(LaunchError::Other(
                "the session host went away (disconnected)".into(),
            )),
        }
    }

    fn close(mut self: Box<Self>) -> Result<(), LaunchError> {
        if let Some(mut pipe) = self.pipe.take() {
            // Best effort: if the host is already gone the pipe closing below ends it all the same.
            let _ = write_frame(&mut pipe, &Request::Close {});
            let _ = read_frame::<_, Response>(&mut pipe);
        }
        // The runner publishes exit.code only after the host's job is dead and its work dir is wiped.
        let results = self.layout.job_results(&self.id);
        let code = results.join("exit.code");
        let start = Instant::now();
        while !code.exists() {
            // Results gone as well: a later `open_session` reaped this ended session first (see
            // `reap_ended_sessions`), so it was already collected. That is the end, not a failure.
            if !results.exists() {
                break;
            }
            if start.elapsed() > GRACE {
                return Err(LaunchError::Other(format!(
                    "the runner did not confirm the session's end within {}s",
                    GRACE.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.remove()
    }
}

impl AccountSession {
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

impl Drop for AccountSession {
    fn drop(&mut self) {
        // Closing the pipe is what ends the host; the spool entry is ours to remove.
        self.pipe = None;
        let _ = self.remove();
    }
}

struct AccountEnv {
    layout: Layout,
    id: String,
    limit: Duration,
}

impl Environment for AccountEnv {
    fn run(&mut self, _spec: &JobSpec) -> Result<Exit, LaunchError> {
        start_task()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn session(tag: &str) -> (Layout, Box<AccountSession>) {
        let root =
            std::env::temp_dir().join(format!("antspawn-close-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = Layout::new(&root);
        let s = AccountSession {
            layout: layout.clone(),
            id: "s-close".into(),
            pipe: None,
            next_id: 1,
        };
        (layout, Box::new(s))
    }

    #[test]
    fn close_on_a_session_the_reaper_already_collected_is_ok_not_a_60s_wait_and_an_error() {
        // Pip review of #75: the reaper may remove an ended session before its owner calls `close`.
        let (_, s) = session("reaped");
        let t = Instant::now();
        s.close()
            .expect("an already-collected session closes cleanly");
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[test]
    fn close_on_an_ended_session_removes_its_entries() {
        // Control: the ordinary path still works and cleans up.
        let (layout, s) = session("ended");
        std::fs::create_dir_all(layout.job_spool("s-close")).unwrap();
        std::fs::create_dir_all(layout.job_results("s-close")).unwrap();
        std::fs::write(layout.job_results("s-close").join("exit.code"), "0").unwrap();
        s.close().unwrap();
        assert!(!layout.job_spool("s-close").exists());
        assert!(!layout.job_results("s-close").exists());
    }
}
