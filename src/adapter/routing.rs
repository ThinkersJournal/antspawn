// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`RoutingDispatch`]: one [`Dispatch`] per launcher id, chosen by the group's `launcher`
//! (`docs/ADAPTER-DECISIONS.md`, decision 3, ruled A).
//!
//! One job at a time across ALL launchers: `busy()` is true while any inner dispatcher is busy, so the
//! watcher polls and mints nothing until the running job ends. Measure before relaxing this to a busy flag
//! per launcher; two launchers may contend for the same host resources.

use std::collections::BTreeMap;

use super::dispatch::Outcome;
use super::github::{RunnerGroup, Secret};
use super::watcher::{Dispatch, QueuedJob};

#[derive(Default)]
pub struct RoutingDispatch {
    by_launcher: BTreeMap<String, Box<dyn Dispatch>>,
}

impl RoutingDispatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the dispatcher for `launcher`. A second registration of one id is a programming error and
    /// is refused rather than silently replacing the first.
    pub fn with(mut self, launcher: &str, d: Box<dyn Dispatch>) -> Result<Self, String> {
        if self.by_launcher.insert(launcher.to_string(), d).is_some() {
            return Err(format!("launcher `{launcher}` registered twice"));
        }
        Ok(self)
    }
}

impl Dispatch for RoutingDispatch {
    fn busy(&self) -> bool {
        self.by_launcher.values().any(|d| d.busy())
    }

    fn finished(&mut self) -> Vec<Outcome> {
        self.by_launcher
            .values_mut()
            .flat_map(|d| d.finished())
            .collect()
    }

    fn dispatch(
        &mut self,
        group: &RunnerGroup,
        job: &QueuedJob,
        jit: Secret,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("a job is already running".into());
        }
        match self.by_launcher.get_mut(&group.launcher) {
            Some(d) => d.dispatch(group, job, jit),
            None => Err(format!(
                "group `{}` names launcher `{}`, which this adapter does not run",
                group.name, group.launcher
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    struct Fake {
        name: &'static str,
        busy: Arc<AtomicBool>,
        log: Arc<Mutex<Vec<String>>>,
    }
    impl Dispatch for Fake {
        fn busy(&self) -> bool {
            self.busy.load(Ordering::SeqCst)
        }
        fn dispatch(&mut self, g: &RunnerGroup, j: &QueuedJob, _: Secret) -> Result<(), String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:{}:{}", self.name, g.name, j.job_id));
            Ok(())
        }
    }

    fn group(name: &str, launcher: &str) -> RunnerGroup {
        RunnerGroup {
            name: name.into(),
            repos: vec!["O/a".into()],
            labels: vec![name.into()],
            launcher: launcher.into(),
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

    fn secret() -> Secret {
        Secret::new("x".into())
    }

    struct Rig {
        r: RoutingDispatch,
        account_busy: Arc<AtomicBool>,
        log: Arc<Mutex<Vec<String>>>,
    }

    fn fake(
        name: &'static str,
        busy: &Arc<AtomicBool>,
        log: &Arc<Mutex<Vec<String>>>,
    ) -> Box<Fake> {
        Box::new(Fake {
            name,
            busy: busy.clone(),
            log: log.clone(),
        })
    }

    fn rig() -> Rig {
        let log = Arc::new(Mutex::new(vec![]));
        let account_busy = Arc::new(AtomicBool::new(false));
        let sandbox_busy = Arc::new(AtomicBool::new(false));
        let r = RoutingDispatch::new()
            .with("account", fake("account", &account_busy, &log))
            .unwrap()
            .with("windows-sandbox", fake("sandbox", &sandbox_busy, &log))
            .unwrap();
        Rig {
            r,
            account_busy,
            log,
        }
    }

    #[test]
    fn a_job_goes_to_the_launcher_its_group_names_and_to_no_other() {
        let mut t = rig();
        t.r.dispatch(&group("ga", "account"), &job(1), secret())
            .unwrap();
        t.r.dispatch(&group("gs", "windows-sandbox"), &job(2), secret())
            .unwrap();
        assert_eq!(
            t.log.lock().unwrap().as_slice(),
            ["account:ga:1", "sandbox:gs:2"]
        );
    }

    #[test]
    fn busy_is_true_while_any_launcher_is_busy_and_blocks_every_launcher() {
        let mut t = rig();
        assert!(!t.r.busy());
        t.account_busy.store(true, Ordering::SeqCst);
        assert!(t.r.busy());
        // Control: the idle sandbox launcher is refused too, because the rule is one job at a time.
        assert!(
            t.r.dispatch(&group("gs", "windows-sandbox"), &job(3), secret())
                .is_err()
        );
        assert!(t.log.lock().unwrap().is_empty());
        t.account_busy.store(false, Ordering::SeqCst);
        assert!(!t.r.busy());
        t.r.dispatch(&group("gs", "windows-sandbox"), &job(3), secret())
            .unwrap();
    }

    #[test]
    fn an_unknown_launcher_is_refused_naming_group_and_launcher() {
        let mut t = rig();
        let e =
            t.r.dispatch(&group("gx", "naive"), &job(4), Secret::new("SECRET".into()))
                .unwrap_err();
        assert!(e.contains("gx") && e.contains("naive"), "{e}");
        assert!(!e.contains("SECRET"));
        assert!(t.log.lock().unwrap().is_empty());
    }

    #[test]
    fn a_launcher_registered_twice_is_refused() {
        let t = rig();
        let again = fake("dup", &Arc::new(AtomicBool::new(false)), &t.log);
        assert!(t.r.with("account", again).is_err());
    }
}
