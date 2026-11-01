// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn adapter`: wires the config, the key, GitHub, the watcher and the routing dispatcher together
//! (ROADMAP row 23 slice c; `docs/ADAPTER-DECISIONS.md`). The logic lives here so it is testable; `main.rs`
//! only parses arguments.
//!
//! Only launchers that can really take a JIT config are registered. `account` is (the pipe path, not run
//! live: ROADMAP row 21). `windows-sandbox` has no transport yet (board 322), so a group naming it makes
//! `--check` fail and the adapter refuse to start, rather than mint a runner that cannot be delivered.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::account_backend::{PerGroupAccountBackend, PipeTransport};
use super::dispatch::{RunnerBackend, SandboxDispatch};
use super::github::{
    AdapterConfig, Backoff, FileKeyProvider, KeyError, KeyProvider, Minter, RunnerGroup, Secret,
    check_key_path,
};
use super::github_client::HttpGithub;
use super::routing::RoutingDispatch;
use super::watcher::{Dispatch, QueuedJob, TickReport, Watcher};

/// Seconds between polls when running continuously.
const POLL_S: u64 = 20;

/// The App key, from a DPAPI-protected file (the default on Windows) or a plain PEM file (`--pem-key`).
pub enum AnyKey {
    #[cfg(windows)]
    Dpapi(super::github_client::DpapiKeyProvider),
    File(FileKeyProvider),
}

impl KeyProvider for AnyKey {
    fn load(&self) -> Result<Secret, KeyError> {
        match self {
            #[cfg(windows)]
            AnyKey::Dpapi(k) => k.load(),
            AnyKey::File(k) => k.load(),
        }
    }
}

pub fn key_provider(path: &Path, pem: bool) -> Result<AnyKey, KeyError> {
    #[cfg(windows)]
    if !pem {
        return super::github_client::DpapiKeyProvider::new(path.to_path_buf(), vec![])
            .map(AnyKey::Dpapi);
    }
    // Off Windows there is no DPAPI, so a plain PEM file is the only form and `--pem-key` changes nothing.
    #[cfg(not(windows))]
    let _ = pem;
    FileKeyProvider::new(path.to_path_buf(), vec![]).map(AnyKey::File)
}

/// Load and validate the configuration file.
pub fn load_config(path: &Path) -> Result<AdapterConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    super::config_file::load(&text, &path.display().to_string()).map_err(|e| e.to_string())
}

/// Can this adapter hand a JIT config to `launcher` today?
fn runnable(launcher: &str) -> Result<(), String> {
    match launcher {
        "account" if cfg!(windows) => Ok(()),
        "account" => Err(
            "the account launcher is Windows-only: this build cannot check or run it here; run `spawn adapter --check` on the Windows host"
                .into(),
        ),
        "windows-sandbox" => {
            Err("the windows-sandbox launcher has no JIT-config transport yet (board 322)".into())
        }
        other => Err(format!("unknown launcher `{other}`")),
    }
}

/// What `--check` finds, one line per fact; `problems` is the subset that stops the adapter.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CheckReport {
    pub lines: Vec<String>,
    pub problems: Vec<String>,
}

/// Offline checks: nothing here touches the network or reads the key.
pub fn check(cfg: &AdapterConfig, pem_key: bool) -> CheckReport {
    let mut r = CheckReport::default();
    r.lines.push(format!(
        "org `{}`, {} group(s), job ceiling {} s",
        cfg.org,
        cfg.groups.len(),
        cfg.job_timeout_s
    ));
    for g in &cfg.groups {
        match runnable(&g.launcher) {
            Ok(()) => r.lines.push(format!(
                "group `{}`: launcher `{}`, {} repo(s), labels {}",
                g.name,
                g.launcher,
                g.repos.len(),
                g.labels.join(",")
            )),
            Err(why) => r.problems.push(format!("group `{}`: {why}", g.name)),
        }
    }
    match check_key_path(&cfg.key_path, &[]) {
        Err(e) => r.problems.push(format!("key_path: {e}")),
        Ok(()) if !cfg.key_path.is_file() => r.problems.push(format!(
            "key_path: {} is not a file",
            cfg.key_path.display()
        )),
        Ok(()) => r.lines.push(format!(
            "key: {} ({}); contents not read by --check",
            cfg.key_path.display(),
            if pem_key || !cfg!(windows) {
                "plain PEM"
            } else {
                "DPAPI"
            }
        )),
    }
    r
}

/// Prints the outcome of each runner when it ends. Holds no secret.
struct Logged<B: RunnerBackend>(B);

impl<B: RunnerBackend> RunnerBackend for Logged<B> {
    fn run_one(
        &self,
        group: &RunnerGroup,
        started: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<String, String> {
        let r = self.0.run_one(group, started);
        match &r {
            Ok(s) => eprintln!("spawn adapter: group `{}` runner ended: {s}", group.name),
            Err(e) => eprintln!("spawn adapter: group `{}` runner failed: {e}", group.name),
        }
        r
    }
}

/// The routing dispatcher for `cfg`: one dispatcher per launcher some group names. Refuses (naming every
/// group) if any group's launcher cannot take a config.
pub fn router(cfg: &AdapterConfig) -> Result<RoutingDispatch, String> {
    let bad: Vec<String> = cfg
        .groups
        .iter()
        .filter_map(|g| {
            runnable(&g.launcher)
                .err()
                .map(|w| format!("group `{}`: {w}", g.name))
        })
        .collect();
    if !bad.is_empty() {
        return Err(bad.join("; "));
    }
    let mut r = RoutingDispatch::new();
    if cfg.groups.iter().any(|g| g.launcher == "account") {
        let backend = Logged(PerGroupAccountBackend {
            limit: Duration::from_secs(cfg.job_timeout_s),
            poll: Duration::from_secs(5),
        });
        let transport = PipeTransport {
            window: Duration::from_secs(60),
        };
        r = r.with(
            "account",
            Box::new(SandboxDispatch::new(backend, transport)),
        )?;
    }
    Ok(r)
}

/// Announces, before each dispatch, which job goes to which group and launcher. Holds no secret: the JIT
/// config is passed through untouched and never formatted.
struct Announce<D: Dispatch> {
    inner: D,
    sink: Box<dyn Fn(String)>,
}

impl<D: Dispatch> Dispatch for Announce<D> {
    fn busy(&self) -> bool {
        self.inner.busy()
    }

    fn finished(&mut self) -> Vec<super::dispatch::Outcome> {
        self.inner.finished()
    }

    fn dispatch(
        &mut self,
        group: &RunnerGroup,
        job: &QueuedJob,
        jit: Secret,
    ) -> Result<(), String> {
        (self.sink)(format!(
            "spawn adapter: job {} ({}): group `{}` -> launcher `{}`",
            job.job_id, job.repo, group.name, group.launcher
        ));
        self.inner.dispatch(group, job, jit)
    }
}

/// Did a `--once` run end well? A failed dispatch, a runner that ended in an error (`settled` is the report
/// taken after the wait), or a runner still going when the wait ran out, is a failure: a gate must not read
/// success from any of them. `--once` never retries. Returns the reasons (empty = success).
pub fn once_failures(t: &TickReport, settled: &TickReport, timed_out: bool) -> Vec<String> {
    let mut f: Vec<String> = t
        .dispatch_failed
        .iter()
        .map(|(id, why)| format!("job {id}: runner minted but not started: {why}"))
        .collect();
    f.extend(
        settled
            .ended_failed
            .iter()
            .map(|(id, why, _)| format!("job {id}: the runner ended in an error: {why}")),
    );
    if timed_out {
        f.push("the runner was still running when the wait ended".into());
    }
    f
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn print_tick(t: &TickReport) {
    for (id, task_status) in &t.ended_ok {
        eprintln!("spawn adapter: job {id}: runner ended: {task_status}");
    }
    for (id, why, retry) in &t.ended_failed {
        let next = if *retry {
            "; will mint again after the cooldown"
        } else {
            ""
        };
        eprintln!("spawn adapter: job {id}: the runner ended in an error: {why}{next}");
    }
    if t.busy {
        eprintln!("spawn adapter: a runner is running; nothing polled");
        return;
    }
    for id in &t.dispatched {
        eprintln!("spawn adapter: job {id}: runner started");
    }
    for (id, why) in &t.dispatch_failed {
        eprintln!("spawn adapter: job {id}: runner minted but not started: {why}");
    }
    for (id, why, operator) in &t.stayed_queued {
        let who = if *operator { " (operator needed)" } else { "" };
        eprintln!("spawn adapter: job {id}: stays queued: {why}{who}");
    }
    for (repo, why) in &t.poll_errors {
        eprintln!("spawn adapter: poll of {repo} failed: {why}");
    }
    if t.unrouted > 0 {
        eprintln!("spawn adapter: {} queued job(s) match no group", t.unrouted);
    }
}

/// Run the adapter. `once`: one poll, then wait for the runner it started (if any) and return.
/// `Ok(true)`: clean. `Ok(false)`: `--once` ended badly (reasons printed to stderr; exit code 3).
pub fn run(config: &Path, pem_key: bool, once: bool) -> Result<bool, String> {
    let cfg = load_config(config)?;
    let dispatch = router(&cfg)?;
    let keys = key_provider(&cfg.key_path, pem_key).map_err(|e| e.to_string())?;
    let github = HttpGithub::new(cfg.app_id, cfg.installation_id, keys);
    let ceiling = Duration::from_secs(cfg.job_timeout_s);
    let mut watcher = Watcher::new(
        cfg,
        Minter::new(&github, Backoff::default()),
        &github,
        Announce {
            inner: dispatch,
            sink: Box::new(|l| eprintln!("{l}")),
        },
    );
    loop {
        let tick = watcher.tick(unix_now());
        print_tick(&tick);
        if once {
            let end = std::time::Instant::now() + ceiling + Duration::from_secs(120);
            while watcher.busy() && std::time::Instant::now() < end {
                std::thread::sleep(Duration::from_secs(1));
            }
            let timed_out = watcher.busy();
            let settled = watcher.settle(unix_now());
            print_tick(&settled);
            let bad = once_failures(&tick, &settled, timed_out);
            for why in &bad {
                eprintln!("spawn adapter: --once failed: {why}");
            }
            return Ok(bad.is_empty());
        }
        std::thread::sleep(Duration::from_secs(POLL_S));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use crate::adapter::watcher::Dispatch;

    fn cfg(launcher: &str, key: &str) -> AdapterConfig {
        let extra = if launcher == "account" {
            "task = \"antspawn-r1\"\npipe = \"antspawn-r1\"\n"
        } else {
            ""
        };
        super::super::config_file::load(
            &format!(
                "org = \"O\"\napp_id = 1\ninstallation_id = 2\nkey_path = '{key}'\njob_timeout_s = 600\n\n[[group]]\nname = \"g\"\nrepos = [\"O/a\"]\nlabels = [\"antspawn\"]\nlauncher = \"{launcher}\"\n{extra}"
            ),
            "t.toml",
        )
        .unwrap()
    }

    #[test]
    fn a_sandbox_group_stops_check_and_start_naming_the_group_and_the_board() {
        let c = cfg("windows-sandbox", "C:/k.pem");
        let r = check(&c, true);
        assert!(
            r.problems
                .iter()
                .any(|p| p.contains("`g`") && p.contains("322"))
        );
        let e = router(&c).err().expect("refused");
        assert!(e.contains("`g`") && e.contains("322"), "{e}");
    }

    #[test]
    fn once_fails_on_a_failed_dispatch_or_a_runner_still_going_and_only_then() {
        let clean = TickReport::default();
        assert!(
            once_failures(&clean, &clean, false).is_empty(),
            "control: a clean tick succeeds"
        );
        let mut bad = TickReport::default();
        bad.dispatch_failed.push((7, "pipe never appeared".into()));
        let f = once_failures(&bad, &clean, false);
        assert!(
            f.len() == 1 && f[0].contains("job 7") && f[0].contains("pipe never appeared"),
            "{f:?}"
        );
        let t = once_failures(&clean, &clean, true);
        assert!(t.len() == 1 && t[0].contains("still running"), "{t:?}");
        let mut ended = TickReport::default();
        ended.ended_failed.push((9, "pipe closed".into(), false));
        let e = once_failures(&clean, &ended, false);
        assert!(
            e.len() == 1 && e[0].contains("job 9") && e[0].contains("pipe closed"),
            "a runner that ended in an error fails --once: {e:?}"
        );
        let mut fine = TickReport::default();
        fine.ended_ok.push((9, "Code(0)".into()));
        assert!(
            once_failures(&clean, &fine, false).is_empty(),
            "control: a runner that ended well does not"
        );
    }

    #[test]
    fn the_routing_decision_is_announced_with_job_group_and_launcher_and_no_secret() {
        use std::sync::{Arc, Mutex};
        struct Ok1;
        impl Dispatch for Ok1 {
            fn busy(&self) -> bool {
                false
            }
            fn dispatch(
                &mut self,
                _: &RunnerGroup,
                _: &QueuedJob,
                _: Secret,
            ) -> Result<(), String> {
                Ok(())
            }
        }
        let lines = Arc::new(Mutex::new(Vec::new()));
        let l2 = lines.clone();
        let mut a = Announce {
            inner: Ok1,
            sink: Box::new(move |l| l2.lock().unwrap().push(l)),
        };
        let c = cfg("windows-sandbox", "C:/k.pem");
        let job = QueuedJob {
            repo: "O/a".into(),
            run_id: 1,
            job_id: 42,
            labels: vec!["antspawn".into()],
        };
        a.dispatch(&c.groups[0], &job, Secret::new("SECRET-JIT".into()))
            .unwrap();
        let got = lines.lock().unwrap().join(
            "
",
        );
        assert!(
            got.contains("job 42") && got.contains("`g`") && got.contains("`windows-sandbox`"),
            "{got}"
        );
        assert!(!got.contains("SECRET"));
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_check_says_the_account_launcher_needs_the_windows_host() {
        let r = check(&cfg("account", "/k.pem"), true);
        assert!(
            r.problems.iter().any(|p| p.contains("Windows host")),
            "{r:?}"
        );
    }

    #[test]
    fn an_unknown_launcher_is_refused() {
        let c = cfg("naive", "C:/k.pem");
        assert!(
            router(&c)
                .err()
                .unwrap()
                .contains("unknown launcher `naive`")
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_account_group_routes_and_check_needs_a_real_absolute_key_file() {
        let c = cfg("account", "C:/definitely/not/here.pem");
        let r = check(&c, true);
        assert!(r.problems.iter().any(|p| p.contains("not a file")), "{r:?}");
        assert!(r.lines.iter().any(|l| l.contains("launcher `account`")));
        let dispatch = router(&c).expect("account is routable on Windows");
        assert!(!dispatch.busy());
        let exe = std::env::current_exe().unwrap();
        let ok = check(&cfg("account", &exe.display().to_string()), true);
        assert!(ok.problems.is_empty(), "{ok:?}");
    }

    #[test]
    fn account_groups_need_their_own_task_and_pipe_and_others_may_not_set_them() {
        let base = |body: &str| {
            format!(
                "org = \"O\"\napp_id = 1\ninstallation_id = 2\nkey_path = 'C:/k'\njob_timeout_s = 600\n{body}"
            )
        };
        let group = |n: &str, l: &str, extra: &str| {
            format!(
                "\n[[group]]\nname = \"{n}\"\nrepos = [\"O/a\"]\nlabels = [\"{n}\"]\nlauncher = \"{l}\"\n{extra}"
            )
        };
        let load = |b: String| super::super::config_file::load(&b, "t.toml");
        let ok =
            |n: &str, t: &str| group(n, "account", &format!("task = \"{t}\"\npipe = \"{t}\"\n"));
        assert!(load(base(&(ok("a", "t1") + &ok("b", "t2")))).is_ok());
        // Each of these is well-typed and refused by validate: the control above shows the shape loads.
        let e = |b: String| load(b).unwrap_err().to_string();
        assert!(e(base(&group("a", "account", ""))).contains("needs `task` and `pipe`"));
        assert!(e(base(&(ok("a", "t1") + &ok("b", "t1")))).contains("repeats"));
        assert!(
            e(base(&group("a", "windows-sandbox", "task = \"t\"\n")))
                .contains("only the account launcher")
        );
    }

    #[test]
    fn a_relative_key_path_is_refused_by_check() {
        let r = check(&cfg("account", "rel/k.pem"), true);
        assert!(r.problems.iter().any(|p| p.starts_with("key_path")));
    }
}
