// SPDX-License-Identifier: MIT OR Apache-2.0
//! The watcher loop: poll the configured repos for queued jobs, route each to a runner group, mint one
//! just-in-time runner, hand it to a [`Dispatch`]. Pure logic: time and every outside call are passed
//! in, so the tests are deterministic and the loop cannot reach GitHub or start a runner by itself.
//!
//! * **One job at a time.** `account` never claims `concurrent`, so while the dispatcher is busy nothing is
//!   polled, and a tick mints at most one runner.
//! * **A job is minted for once, unless it provably never started.** Its id goes in `jobs` as soon as a
//!   runner is minted (even if the dispatch then fails, so a broken hand-off cannot mint a runner per
//!   tick); the minted-but-unused runner is ephemeral and GitHub expires it. A failed mint is not
//!   recorded: the job stays queued and the [`Minter`] backoff spaces the retries.
//! * **Retry (ROADMAP row 24).** When the dispatcher reports (through [`Dispatch::finished`]) that a job
//!   ended with an error and `nothing_ran` (the environment never started, or the config never arrived,
//!   so no runner took the job), the job is minted for again after `cooldown_s`, at most `max_attempts`
//!   times in all. Any other failure is final: a runner that started may have taken work.
//! * **Unrouted jobs are left alone.** Another runner pool may own them; they are never recorded.
//! * The JIT config is a [`Secret`]: it goes from the minter to the dispatcher and nowhere else, and no
//!   report type holds it.

use std::collections::{HashMap, HashSet};

use super::dispatch::Outcome;
use super::github::{
    AdapterConfig, GithubApi, JitRequest, MintError, MintOutcome, Minter, RunnerGroup, Secret,
};

/// A job GitHub reports as queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedJob {
    /// `owner/name`.
    pub repo: String,
    pub run_id: u64,
    pub job_id: u64,
    /// The job's `runs-on` labels.
    pub labels: Vec<String>,
}

/// Where queued jobs come from (the real one polls `/repos/{repo}/actions/runs` and their jobs).
pub trait JobSource {
    fn queued(&self, repo: &str) -> Result<Vec<QueuedJob>, MintError>;
}

impl<T: JobSource + ?Sized> JobSource for &T {
    fn queued(&self, repo: &str) -> Result<Vec<QueuedJob>, MintError> {
        (**self).queued(repo)
    }
}

/// Where a minted runner goes: starts it inside a fresh environment of the group's launcher.
pub trait Dispatch {
    /// Is a job still running? Nothing is polled or minted while this is true.
    fn busy(&self) -> bool;
    fn dispatch(&mut self, group: &RunnerGroup, job: &QueuedJob, jit: Secret)
    -> Result<(), String>;
    /// How jobs dispatched earlier ended, each once. The default reports none (a dispatcher that cannot
    /// tell is never retried).
    fn finished(&mut self) -> Vec<Outcome> {
        Vec::new()
    }
}

/// How many times a job whose runner never started is minted for, and how long to wait between attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts in all, the first included. `1` turns retry off.
    pub max_attempts: u32,
    /// Seconds to wait after a failed attempt before minting again.
    pub cooldown_s: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 3,
            cooldown_s: 120,
        }
    }
}

/// What the watcher remembers about a job it minted for.
#[derive(Debug, Clone, Copy)]
struct JobState {
    attempts: u32,
    /// Set only after a failure with `nothing_ran` and attempts left: mint again at or after this time.
    retry_at: Option<u64>,
}

/// What one tick did. Holds no secret.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    /// The dispatcher was busy, so nothing was polled.
    pub busy: bool,
    /// Job ids a runner was minted and dispatched for.
    pub dispatched: Vec<u64>,
    /// `(job id, reason)`: a runner was minted but the dispatch failed (operator needed).
    pub dispatch_failed: Vec<(u64, String)>,
    /// `(job id, one-line account)`: a dispatched runner ended well.
    pub ended_ok: Vec<(u64, String)>,
    /// `(job id, reason, will retry)`: a dispatched job ended in an error. `will retry` is true only when
    /// nothing ran and attempts remain; the job is then minted for again after the cooldown.
    pub ended_failed: Vec<(u64, String, bool)>,
    /// `(job id, reason, operator)`: no runner was minted, the job stays queued.
    pub stayed_queued: Vec<(u64, String, bool)>,
    /// `(repo, reason)`: the poll failed; other repos were still polled.
    pub poll_errors: Vec<(String, String)>,
    /// Queued jobs no group routes to.
    pub unrouted: usize,
}

pub struct Watcher<A: GithubApi, S: JobSource, D: Dispatch> {
    cfg: AdapterConfig,
    minter: Minter<A>,
    source: S,
    dispatch: D,
    jobs: HashMap<u64, JobState>,
    retry: RetryPolicy,
}

impl<A: GithubApi, S: JobSource, D: Dispatch> Watcher<A, S, D> {
    pub fn new(cfg: AdapterConfig, minter: Minter<A>, source: S, dispatch: D) -> Self {
        Watcher {
            cfg,
            minter,
            source,
            dispatch,
            jobs: HashMap::new(),
            retry: RetryPolicy::default(),
        }
    }

    /// Take in how earlier dispatches ended, without polling or minting anything: the part of a tick
    /// that `--once` runs after it has waited for the runner.
    pub fn settle(&mut self, now_s: u64) -> TickReport {
        let mut report = TickReport::default();
        self.absorb(now_s, &mut report);
        report
    }

    fn absorb(&mut self, now_s: u64, report: &mut TickReport) {
        for o in self.dispatch.finished() {
            match o.result {
                Ok(account) => report.ended_ok.push((o.job_id, account)),
                Err(why) => {
                    let will_retry = match self.jobs.get_mut(&o.job_id) {
                        Some(st) if o.nothing_ran && st.attempts < self.retry.max_attempts => {
                            st.retry_at = Some(now_s.saturating_add(self.retry.cooldown_s));
                            true
                        }
                        _ => false,
                    };
                    report.ended_failed.push((o.job_id, why, will_retry));
                }
            }
        }
    }

    /// Is a runner still running?
    pub fn busy(&self) -> bool {
        self.dispatch.busy()
    }

    /// Job ids remembered as already minted for.
    pub fn seen(&self) -> usize {
        self.jobs.len()
    }

    /// The repos any group names, each once, in config order.
    fn repos(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for g in &self.cfg.groups {
            for r in &g.repos {
                if !out.contains(r) {
                    out.push(r.clone());
                }
            }
        }
        out
    }

    pub fn tick(&mut self, now_s: u64) -> TickReport {
        let mut report = TickReport::default();
        if self.dispatch.busy() {
            report.busy = true;
            return report;
        }
        self.absorb(now_s, &mut report);
        let mut queued: Vec<QueuedJob> = Vec::new();
        let mut polled_all = true;
        for repo in self.repos() {
            match self.source.queued(&repo) {
                Ok(jobs) => queued.extend(jobs),
                Err(e) => {
                    polled_all = false;
                    report.poll_errors.push((repo, e.to_string()));
                }
            }
        }
        if polled_all {
            // Forget jobs GitHub no longer lists as queued (taken, finished or cancelled).
            let still: HashSet<u64> = queued.iter().map(|j| j.job_id).collect();
            self.jobs.retain(|id, _| still.contains(id));
        }
        // Oldest first: job ids grow with time.
        queued.sort_by_key(|j| j.job_id);
        for job in queued {
            match self.jobs.get(&job.job_id) {
                // Minted for before. Skip it, unless a failed attempt left it waiting out its cooldown.
                Some(JobState {
                    retry_at: Some(t), ..
                }) if now_s >= *t => {}
                Some(_) => continue,
                None => {}
            }
            let Some(group) = self.cfg.route(&job.repo, &job.labels).cloned() else {
                report.unrouted += 1;
                continue;
            };
            let req = JitRequest {
                org: self.cfg.org.clone(),
                group: group.name.clone(),
                runner_name: format!("antspawn-{}-{}", job.run_id, job.job_id),
                labels: group.labels.clone(),
            };
            match self.minter.mint(now_s, &group, &req) {
                MintOutcome::Minted(jit) => {
                    let attempts = self.jobs.get(&job.job_id).map_or(0, |s| s.attempts) + 1;
                    self.jobs.insert(
                        job.job_id,
                        JobState {
                            attempts,
                            retry_at: None,
                        },
                    );
                    match self.dispatch.dispatch(&group, &job, jit) {
                        Ok(()) => report.dispatched.push(job.job_id),
                        Err(e) => report.dispatch_failed.push((job.job_id, e)),
                    }
                }
                MintOutcome::StayQueued {
                    reason, operator, ..
                } => report.stayed_queued.push((job.job_id, reason, operator)),
            }
            // One job per tick, whatever happened to it.
            break;
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::github::{Backoff, GroupInfo};
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;

    const CFG: &str = r#"
org = "O"
app_id = 1
installation_id = 2
key_path = "C:/ProgramData/antspawn-watcher/app.pem"
job_timeout_s = 3600

[[group]]
name = "g"
repos = ["O/a", "O/b"]
labels = ["antspawn", "account"]
launcher = "account"
task = "antspawn-r1"
pipe = "antspawn-r1"
"#;

    fn job(repo: &str, id: u64, labels: &[&str]) -> QueuedJob {
        QueuedJob {
            repo: repo.into(),
            run_id: id * 10,
            job_id: id,
            labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }

    const MINE: &[&str] = &["self-hosted", "antspawn", "account"];

    #[derive(Default)]
    struct Api {
        mint_calls: Cell<u32>,
        errs: RefCell<Vec<MintError>>,
    }
    impl GithubApi for Rc<Api> {
        fn runner_group(&self, _: &str, _: &str) -> Result<GroupInfo, MintError> {
            Ok(GroupInfo {
                visibility: "selected".into(),
                repos: vec!["O/a".into(), "O/b".into()],
            })
        }
        fn generate_jitconfig(&self, _: &JitRequest) -> Result<Secret, MintError> {
            self.mint_calls.set(self.mint_calls.get() + 1);
            match self.errs.borrow_mut().pop() {
                Some(e) => Err(e),
                None => Ok(Secret::new("jit".into())),
            }
        }
    }

    #[derive(Default)]
    struct Source {
        jobs: RefCell<HashMap<String, Result<Vec<QueuedJob>, String>>>,
        polls: Cell<u32>,
    }
    impl JobSource for Rc<Source> {
        fn queued(&self, repo: &str) -> Result<Vec<QueuedJob>, MintError> {
            self.polls.set(self.polls.get() + 1);
            match self.jobs.borrow().get(repo) {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(e)) => Err(MintError::Unavailable(e.clone())),
                None => Ok(vec![]),
            }
        }
    }

    #[derive(Default)]
    struct Sink {
        busy: Cell<bool>,
        fail: Cell<bool>,
        got: RefCell<Vec<(String, u64, String)>>,
        ended: RefCell<Vec<Outcome>>,
    }
    impl Dispatch for Rc<Sink> {
        fn busy(&self) -> bool {
            self.busy.get()
        }
        fn finished(&mut self) -> Vec<Outcome> {
            std::mem::take(&mut *self.ended.borrow_mut())
        }
        fn dispatch(&mut self, g: &RunnerGroup, j: &QueuedJob, jit: Secret) -> Result<(), String> {
            assert_eq!(jit.expose(), "jit");
            self.got
                .borrow_mut()
                .push((g.launcher.clone(), j.job_id, j.repo.clone()));
            if self.fail.get() {
                Err("no environment".into())
            } else {
                Ok(())
            }
        }
    }

    struct Rig {
        w: Watcher<Rc<Api>, Rc<Source>, Rc<Sink>>,
        api: Rc<Api>,
        src: Rc<Source>,
        sink: Rc<Sink>,
    }

    fn rig() -> Rig {
        let (api, src, sink) = (
            Rc::new(Api::default()),
            Rc::new(Source::default()),
            Rc::new(Sink::default()),
        );
        let w = Watcher::new(
            AdapterConfig::from_toml(CFG).unwrap(),
            Minter::new(
                api.clone(),
                Backoff {
                    min_gap_s: 5,
                    max_gap_s: 60,
                },
            ),
            src.clone(),
            sink.clone(),
        );
        Rig { w, api, src, sink }
    }

    fn set(r: &Rig, repo: &str, jobs: Vec<QueuedJob>) {
        r.src.jobs.borrow_mut().insert(repo.into(), Ok(jobs));
    }

    #[test]
    fn a_routed_job_gets_one_runner_for_its_groups_launcher() {
        let mut r = rig();
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        let rep = r.w.tick(0);
        assert_eq!(rep.dispatched, vec![7]);
        assert_eq!(
            *r.sink.got.borrow(),
            vec![("account".into(), 7, "O/a".into())]
        );
        assert_eq!(r.api.mint_calls.get(), 1);
    }

    #[test]
    fn a_job_no_group_routes_is_left_alone_and_costs_no_mint() {
        let mut r = rig();
        set(
            &r,
            "O/a",
            vec![
                job("O/a", 1, &["self-hosted", "linux"]),
                job("O/a", 2, &["antspawn"]),
            ],
        );
        let rep = r.w.tick(0);
        assert_eq!(rep.unrouted, 2);
        assert!(rep.dispatched.is_empty());
        assert_eq!(r.api.mint_calls.get(), 0);
        assert_eq!(r.w.seen(), 0, "unrouted jobs are not remembered");
    }

    #[test]
    fn nothing_is_polled_while_a_job_runs() {
        let mut r = rig();
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        r.sink.busy.set(true);
        let rep = r.w.tick(0);
        assert!(rep.busy);
        assert_eq!(r.src.polls.get(), 0);
        assert_eq!(r.api.mint_calls.get(), 0);
    }

    #[test]
    fn one_runner_per_tick_oldest_first_then_the_next() {
        let mut r = rig();
        set(&r, "O/a", vec![job("O/a", 9, MINE)]);
        set(&r, "O/b", vec![job("O/b", 4, MINE)]);
        assert_eq!(r.w.tick(0).dispatched, vec![4]);
        assert_eq!(r.w.tick(1).dispatched, vec![9]);
        assert_eq!(r.api.mint_calls.get(), 2);
    }

    #[test]
    fn a_job_still_queued_is_not_minted_for_twice() {
        let mut r = rig();
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        assert_eq!(r.w.tick(0).dispatched, vec![7]);
        let again = r.w.tick(1);
        assert!(again.dispatched.is_empty());
        assert_eq!(
            r.api.mint_calls.get(),
            1,
            "known-bad control: a watcher without `seen` mints twice"
        );
    }

    #[test]
    fn a_failed_mint_is_retried_after_the_cooldown_and_not_before() {
        let mut r = rig();
        *r.api.errs.borrow_mut() = vec![MintError::Unavailable("503".into())];
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        let first = r.w.tick(0);
        assert_eq!(first.stayed_queued.len(), 1);
        assert!(!first.stayed_queued[0].2, "an outage needs no operator");
        assert_eq!(r.w.seen(), 0, "a failed mint is not recorded");
        let inside = r.w.tick(1);
        assert_eq!(inside.stayed_queued.len(), 1);
        assert_eq!(
            r.api.mint_calls.get(),
            1,
            "inside the cooldown the API is not called"
        );
        assert_eq!(r.w.tick(10).dispatched, vec![7]);
    }

    #[test]
    fn a_failed_dispatch_is_reported_and_never_re_minted() {
        let mut r = rig();
        r.sink.fail.set(true);
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        let rep = r.w.tick(0);
        assert_eq!(rep.dispatch_failed, vec![(7, "no environment".to_string())]);
        assert!(r.w.tick(100).dispatched.is_empty());
        assert!(r.w.tick(200).dispatched.is_empty());
        assert_eq!(r.api.mint_calls.get(), 1);
    }

    /// Report job `id` as ended with `result`; `nothing_ran` as the dispatcher would set it.
    fn end(r: &Rig, id: u64, result: Result<&str, &str>, nothing_ran: bool) {
        r.sink.ended.borrow_mut().push(Outcome {
            job_id: id,
            result: result.map(String::from).map_err(String::from),
            nothing_ran,
        });
    }

    fn policy(r: &mut Rig, max_attempts: u32, cooldown_s: u64) {
        r.w.retry = RetryPolicy {
            max_attempts,
            cooldown_s,
        };
    }

    #[test]
    fn a_job_whose_runner_never_started_is_minted_for_again_after_the_cooldown_up_to_the_cap() {
        let mut r = rig();
        policy(&mut r, 3, 60);
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        assert_eq!(r.w.tick(0).dispatched, vec![7]);
        end(&r, 7, Err("could not open pipe p: NotFound"), true);
        let t = r.w.tick(10);
        assert_eq!(
            t.ended_failed,
            vec![(7, "could not open pipe p: NotFound".to_string(), true)]
        );
        assert!(t.dispatched.is_empty(), "inside the cooldown: no mint");
        assert_eq!(r.api.mint_calls.get(), 1);
        assert!(r.w.tick(69).dispatched.is_empty(), "one second early");
        assert_eq!(r.w.tick(70).dispatched, vec![7], "cooldown over: attempt 2");
        end(&r, 7, Err("again"), true);
        assert!(r.w.tick(71).ended_failed[0].2, "attempt 2 of 3 may retry");
        assert_eq!(r.w.tick(131).dispatched, vec![7], "attempt 3");
        end(&r, 7, Err("third"), true);
        let last = r.w.tick(132);
        assert_eq!(
            last.ended_failed,
            vec![(7, "third".to_string(), false)],
            "attempt 3 of 3 is final"
        );
        assert!(r.w.tick(10_000).dispatched.is_empty());
        assert_eq!(r.api.mint_calls.get(), 3, "capped at max_attempts");
    }

    #[test]
    fn a_runner_that_started_and_then_failed_is_never_minted_for_again() {
        let mut r = rig();
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        assert_eq!(r.w.tick(0).dispatched, vec![7]);
        end(&r, 7, Err("vm died"), false);
        let t = r.w.tick(1);
        assert_eq!(t.ended_failed, vec![(7, "vm died".to_string(), false)]);
        assert!(r.w.tick(100_000).dispatched.is_empty());
        assert_eq!(
            r.api.mint_calls.get(),
            1,
            "known-bad control: a watcher that retried on any error would mint twice"
        );
    }

    #[test]
    fn max_attempts_one_turns_retry_off_and_an_ok_end_is_reported_not_retried() {
        let mut r = rig();
        policy(&mut r, 1, 0);
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        assert_eq!(r.w.tick(0).dispatched, vec![7]);
        end(&r, 7, Err("no pipe"), true);
        assert!(!r.w.tick(1).ended_failed[0].2);
        assert!(r.w.tick(2).dispatched.is_empty());
        let mut r = rig();
        set(&r, "O/a", vec![job("O/a", 8, MINE)]);
        assert_eq!(r.w.tick(0).dispatched, vec![8]);
        end(&r, 8, Ok("Code(0)"), false);
        let t = r.w.tick(1);
        assert_eq!(t.ended_ok, vec![(8, "Code(0)".to_string())]);
        assert!(t.ended_failed.is_empty());
    }

    #[test]
    fn a_job_that_left_the_queue_while_waiting_to_retry_is_not_minted_for_again() {
        let mut r = rig();
        policy(&mut r, 3, 60);
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        assert_eq!(r.w.tick(0).dispatched, vec![7]);
        end(&r, 7, Err("no pipe"), true);
        assert!(r.w.tick(1).ended_failed[0].2);
        // Another runner took it (GitHub no longer lists it as queued).
        set(&r, "O/a", vec![]);
        assert!(r.w.tick(2).dispatched.is_empty());
        assert_eq!(r.w.seen(), 0, "forgotten");
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        assert_eq!(
            r.w.tick(1000).dispatched,
            vec![7],
            "if it is queued again it is a new job to us"
        );
    }

    #[test]
    fn settle_takes_in_how_a_job_ended_without_polling_or_minting() {
        let mut r = rig();
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        assert_eq!(r.w.tick(0).dispatched, vec![7]);
        let polls = r.src.polls.get();
        end(&r, 7, Err("pipe closed"), false);
        let s = r.w.settle(5);
        assert_eq!(s.ended_failed, vec![(7, "pipe closed".to_string(), false)]);
        assert_eq!(r.src.polls.get(), polls, "no poll");
        assert_eq!(r.api.mint_calls.get(), 1, "no mint");
    }

    #[test]
    fn a_failing_repo_does_not_stop_the_others() {
        let mut r = rig();
        r.src
            .jobs
            .borrow_mut()
            .insert("O/a".into(), Err("503".into()));
        set(&r, "O/b", vec![job("O/b", 3, MINE)]);
        let rep = r.w.tick(0);
        assert_eq!(rep.poll_errors.len(), 1);
        assert_eq!(rep.poll_errors[0].0, "O/a");
        assert_eq!(rep.dispatched, vec![3]);
    }

    #[test]
    fn seen_forgets_jobs_github_no_longer_lists_but_not_after_a_partial_poll() {
        let mut r = rig();
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        assert_eq!(r.w.tick(0).dispatched, vec![7]);
        assert_eq!(r.w.seen(), 1);
        // A poll that failed for one repo cannot say the job is gone.
        r.src
            .jobs
            .borrow_mut()
            .insert("O/a".into(), Err("503".into()));
        assert_eq!(r.w.tick(1).poll_errors.len(), 1);
        assert_eq!(r.w.seen(), 1);
        // A complete poll that no longer lists it forgets it.
        set(&r, "O/a", vec![]);
        assert!(r.w.tick(2).poll_errors.is_empty());
        assert_eq!(r.w.seen(), 0);
    }

    #[test]
    fn the_report_carries_no_secret() {
        let mut r = rig();
        set(&r, "O/a", vec![job("O/a", 7, MINE)]);
        let rep = r.w.tick(0);
        assert!(!format!("{rep:?}").contains("jit"));
    }
}
