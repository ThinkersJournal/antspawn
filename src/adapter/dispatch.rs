// SPDX-License-Identifier: MIT OR Apache-2.0
//! The real [`Dispatch`]: one runner VM per minted JIT config, on a background thread so the watcher
//! loop stays responsive while `busy()` holds it to one job at a time (Windows Sandbox runs one VM at
//! a time on this host, MEASUREMENTS 2026-10-29).
//!
//! Two seams keep this testable without a VM and keep board 322 open:
//! * [`RunnerBackend`] runs one VM from start to destroy and calls a hook with the sandbox id once it is
//!   up ([`WsbBackend`] is the real one, built on `RunnerVm`).
//! * [`JitTransport`] is how the config reaches the runner inside that VM. **No real transport exists
//!   yet**: `wsb exec` forwards no stdin (measured 0.0.35) and the replacement waits on CireSnave's answer
//!   to board 322. Whatever it is, it receives the [`Secret`] by reference and must keep it out of files,
//!   arguments and variables (board 260).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::github::{RunnerGroup, Secret};
use super::watcher::{Dispatch, QueuedJob};

/// Gets the JIT config from the host to the runner inside sandbox `sandbox_id`.
pub trait JitTransport: Send + Sync + 'static {
    fn deliver(&self, sandbox_id: &str, jit: &Secret) -> Result<(), String>;
}

/// Runs one runner VM, start to destroy. `started` is called with the sandbox id once the VM is up; if it
/// fails the VM is closed and its error returned. `Ok` carries a one-line account of how the runner ended.
pub trait RunnerBackend: Send + Sync + 'static {
    fn run_one(
        &self,
        group: &RunnerGroup,
        started: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<String, String>;
}

/// How one dispatched job ended. Holds no secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub job_id: u64,
    pub result: Result<String, String>,
}

pub struct SandboxDispatch<B: RunnerBackend, T: JitTransport> {
    backend: Arc<B>,
    transport: Arc<T>,
    busy: Arc<AtomicBool>,
    outcomes: Arc<Mutex<Vec<Outcome>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Clears `busy` when the job thread ends, even by panic.
struct ClearOnDrop(Arc<AtomicBool>);
impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl<B: RunnerBackend, T: JitTransport> SandboxDispatch<B, T> {
    pub fn new(backend: B, transport: T) -> Self {
        SandboxDispatch {
            backend: Arc::new(backend),
            transport: Arc::new(transport),
            busy: Arc::new(AtomicBool::new(false)),
            outcomes: Arc::new(Mutex::new(Vec::new())),
            thread: None,
        }
    }

    /// Outcomes of finished jobs since the last call.
    pub fn take_outcomes(&self) -> Vec<Outcome> {
        std::mem::take(&mut *self.outcomes.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Wait for the running job, if any (shutdown and tests).
    pub fn join(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl<B: RunnerBackend, T: JitTransport> Dispatch for SandboxDispatch<B, T> {
    fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn dispatch(
        &mut self,
        group: &RunnerGroup,
        job: &QueuedJob,
        jit: Secret,
    ) -> Result<(), String> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err("a runner VM is already running".into());
        }
        self.join();
        let (backend, transport) = (self.backend.clone(), self.transport.clone());
        let (busy, outcomes) = (self.busy.clone(), self.outcomes.clone());
        let (group, job_id) = (group.clone(), job.job_id);
        self.thread = Some(std::thread::spawn(move || {
            let _clear = ClearOnDrop(busy);
            let result = backend.run_one(&group, &mut |id| transport.deliver(id, &jit));
            drop(jit);
            outcomes
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(Outcome { job_id, result });
        }));
        Ok(())
    }
}

/// The real backend: a Windows Sandbox runner VM with the pinned runner mapped in.
pub struct WsbBackend {
    pub runner_dir: PathBuf,
    /// The guest command line that starts the runner (and its JIT-config receiver); see `runner_job_script`.
    pub start: String,
    /// Longest a runner VM may live (`AdapterConfig::clamp_time`).
    pub limit: Duration,
}

#[cfg(windows)]
impl RunnerBackend for WsbBackend {
    fn run_one(
        &self,
        _group: &RunnerGroup,
        started: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<String, String> {
        use crate::launcher::LaunchError;
        use crate::launchers::windows_sandbox::RunnerVm;

        let mut vm =
            RunnerVm::provision(&self.runner_dir, &self.start).map_err(|e| e.to_string())?;
        let ran = vm.run(Some(self.limit), &mut |id| {
            started(id).map_err(LaunchError::Other)
        });
        let summary = ran
            .map(|exit| format!("{exit:?}"))
            .map_err(|e| e.to_string());
        let destroyed = vm.destroy().map_err(|e| e.to_string());
        match (summary, destroyed) {
            (Ok(s), Ok(())) => Ok(s),
            (Ok(s), Err(d)) => Err(format!("runner ended ({s}) but destroy failed: {d}")),
            (Err(e), _) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::github::RunnerGroup;
    use std::sync::mpsc;

    fn group() -> RunnerGroup {
        RunnerGroup {
            name: "g".into(),
            repos: vec!["O/a".into()],
            labels: vec!["antspawn".into()],
            launcher: "windows-sandbox".into(),
            task: None,
            pipe: None,
        }
    }

    fn job(id: u64) -> QueuedJob {
        QueuedJob {
            repo: "O/a".into(),
            run_id: id * 10,
            job_id: id,
            labels: vec![],
        }
    }

    /// Records what the transport saw; a `gate` lets a test hold the VM open.
    struct Fake {
        gate: Mutex<mpsc::Receiver<()>>,
        fail_start: bool,
    }
    impl RunnerBackend for Fake {
        fn run_one(
            &self,
            _: &RunnerGroup,
            started: &mut dyn FnMut(&str) -> Result<(), String>,
        ) -> Result<String, String> {
            started("sb-1")?;
            let _ = self.gate.lock().unwrap().recv();
            if self.fail_start {
                Err("vm died".into())
            } else {
                Ok("Code(0)".into())
            }
        }
    }
    struct Seen(Mutex<Vec<(String, String)>>, bool);
    impl JitTransport for Arc<Seen> {
        fn deliver(&self, id: &str, jit: &Secret) -> Result<(), String> {
            if self.1 {
                return Err("no route".into());
            }
            self.0
                .lock()
                .unwrap()
                .push((id.into(), jit.expose().into()));
            Ok(())
        }
    }

    fn rig(
        seen_fails: bool,
        vm_fails: bool,
    ) -> (
        SandboxDispatch<Fake, Arc<Seen>>,
        mpsc::Sender<()>,
        Arc<Seen>,
    ) {
        let (tx, rx) = mpsc::channel();
        let seen = Arc::new(Seen(Mutex::new(vec![]), seen_fails));
        let d = SandboxDispatch::new(
            Fake {
                gate: Mutex::new(rx),
                fail_start: vm_fails,
            },
            seen.clone(),
        );
        (d, tx, seen)
    }

    #[test]
    fn busy_from_dispatch_until_the_vm_ends_and_the_config_reaches_the_transport_once() {
        let (mut d, gate, seen) = rig(false, false);
        assert!(!d.busy());
        d.dispatch(&group(), &job(7), Secret::new("JIT".into()))
            .unwrap();
        assert!(d.busy(), "busy while the VM is held open");
        assert!(
            d.dispatch(&group(), &job(8), Secret::new("X".into()))
                .is_err(),
            "a second job is refused"
        );
        gate.send(()).unwrap();
        d.join();
        assert!(!d.busy());
        assert_eq!(
            seen.0.lock().unwrap().as_slice(),
            [("sb-1".to_string(), "JIT".to_string())]
        );
        assert_eq!(
            d.take_outcomes(),
            vec![Outcome {
                job_id: 7,
                result: Ok("Code(0)".into())
            }]
        );
        assert!(d.take_outcomes().is_empty(), "outcomes are taken once");
    }

    #[test]
    fn a_failed_delivery_is_the_outcome_and_frees_the_dispatcher() {
        let (mut d, gate, _seen) = rig(true, false);
        d.dispatch(&group(), &job(1), Secret::new("JIT".into()))
            .unwrap();
        let _ = gate.send(());
        d.join();
        assert!(!d.busy());
        assert_eq!(
            d.take_outcomes(),
            vec![Outcome {
                job_id: 1,
                result: Err("no route".into())
            }]
        );
    }

    #[test]
    fn a_vm_failure_is_reported_and_the_next_job_can_run() {
        let (mut d, gate, _seen) = rig(false, true);
        d.dispatch(&group(), &job(1), Secret::new("A".into()))
            .unwrap();
        gate.send(()).unwrap();
        d.join();
        assert_eq!(d.take_outcomes()[0].result, Err("vm died".into()));
        d.dispatch(&group(), &job(2), Secret::new("B".into()))
            .unwrap();
        gate.send(()).unwrap();
        d.join();
        assert_eq!(d.take_outcomes()[0].job_id, 2);
    }

    #[test]
    fn outcomes_never_carry_the_config() {
        let (mut d, gate, _seen) = rig(false, false);
        d.dispatch(&group(), &job(1), Secret::new("SECRET-JIT".into()))
            .unwrap();
        gate.send(()).unwrap();
        d.join();
        assert!(!format!("{:?}", d.take_outcomes()).contains("SECRET-JIT"));
    }
}
