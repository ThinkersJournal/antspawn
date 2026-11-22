// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn adapter`: wires the config, the key, GitHub, the watcher and the routing dispatcher together
//! (ROADMAP row 23 slice c; `docs/ADAPTER-DECISIONS.md`). The logic lives here so it is testable; `main.rs`
//! only parses arguments.
//!
//! Only launchers that can really take a JIT config are registered. `account` is (the pipe path, not run
//! live: ROADMAP row 21). `windows-sandbox` is refused: a runner needs network, and CireSnave ruled
//! 2026-11-21 that network-needing runners run in a VM behind a filtering proxy, not in Sandbox (ROADMAP rows 38 to 44,
//! `docs/RUNNER-NETWORK.md`), so a group naming it makes `--check` fail and the adapter refuse to start.

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

/// Where a config's egress approvals live: `egress-approved.toml` beside the config file.
pub fn approvals_path(config: &Path) -> std::path::PathBuf {
    config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("egress-approved.toml")
}

/// Each group's effective egress list. A missing approvals file is an empty one: a group with no extras needs none,
/// and a group with an extra is refused for want of its approval. An unreadable or malformed file is an error.
pub fn load_egress(
    config: &Path,
    cfg: &AdapterConfig,
) -> Result<Vec<super::egress_policy::GroupEgress>, String> {
    let path = approvals_path(config);
    let approvals = match std::fs::read_to_string(&path) {
        Ok(t) => super::egress_policy::Approvals::from_toml(&t)
            .map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    super::egress_policy::effective(cfg, &approvals)
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
            Err("the windows-sandbox launcher is not used for runners: network-needing runners run in a VM behind a filtering proxy (CireSnave 2026-11-21; ROADMAP rows 38 to 44)".into())
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

/// Offline checks: nothing here touches the network or reads the key (see [`check_with_key`] for the load).
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
    if cfg.groups.iter().any(|g| g.launcher == "hyperv-vm") {
        match super::guest_credential::default_path()
            .ok_or_else(|| "guest credential: LOCALAPPDATA is not set".to_string())
            .and_then(|p| super::guest_credential::check(&p))
        {
            Ok(line) => r.lines.push(line),
            Err(why) => r.problems.push(why),
        }
    }
    match check_key_path(&cfg.key_path, &[]) {
        Err(e) => r.problems.push(format!("key_path: {e}")),
        Ok(()) if !cfg.key_path.is_file() => r.problems.push(format!(
            "key_path: {} is not a file",
            cfg.key_path.display()
        )),
        Ok(()) => r.lines.push(format!(
            "key: {} ({}); path only; the load check follows",
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

/// Does the key LOAD, and does it look like a PEM private key? Reports the length only: the key is never
/// printed, logged or compared by content, and the error text never carries key material.
pub fn key_loads<K: KeyProvider>(keys: &K) -> Result<String, String> {
    let secret = keys.load().map_err(|e| e.to_string())?;
    let pem = secret.expose();
    let t = pem.trim();
    if t.starts_with("-----BEGIN ") && t.contains("PRIVATE KEY-----") {
        Ok(format!(
            "loads, {} bytes, PEM private-key header present",
            t.len()
        ))
    } else {
        Err("loads but is not a PEM private key".into())
    }
}

/// [`check`] plus the one step that reads the key: it must load through the configured provider (DPAPI as
/// the current user by default, so a run as another account fails here, as it would at start). Skipped when
/// `check` already found the key path unusable. Offline: no network.
pub fn check_with_key(cfg: &AdapterConfig, pem_key: bool) -> CheckReport {
    let mut r = check(cfg, pem_key);
    if r.problems.iter().any(|p| p.starts_with("key_path:")) {
        return r;
    }
    match key_provider(&cfg.key_path, pem_key)
        .map_err(|e| e.to_string())
        .and_then(|k| key_loads(&k))
    {
        Ok(what) => r.lines.push(format!("key: {what}")),
        Err(why) => r.problems.push(format!("key: {why}")),
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

/// Best effort: a status file that cannot be written is reported once per tick and never stops the adapter.
fn write_status<A: super::github::GithubApi, S: super::watcher::JobSource, D: Dispatch>(
    path: Option<&Path>,
    groups: &[String],
    watcher: &Watcher<A, S, D>,
) {
    let Some(p) = path else { return };
    let s = super::status::Status {
        tick_unix: unix_now(),
        pid: std::process::id(),
        groups: groups.iter().map(String::as_str).collect(),
        jobs_in_flight: u32::from(watcher.busy()),
    };
    if let Err(e) = super::status::write(p, &s) {
        eprintln!("spawn adapter: cannot write the status file: {e}");
    }
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
pub fn run(
    config: &Path,
    pem_key: bool,
    once: bool,
    status: Option<&Path>,
) -> Result<bool, String> {
    let cfg = load_config(config)?;
    load_egress(config, &cfg)?; // an unapproved egress entry stops the start, as `--check` reports it
    // one adapter at a time: the lock sits beside the status file and is held until this process ends
    let _lock = status.map(super::status::Lock::acquire).transpose()?;
    let groups: Vec<String> = cfg.groups.iter().map(|g| g.name.clone()).collect();
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
        write_status(status, &groups, &watcher);
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

    /// `None` stands for a provider that cannot decrypt.
    struct FakeKey(Option<&'static str>);
    impl KeyProvider for FakeKey {
        fn load(&self) -> Result<Secret, KeyError> {
            self.0
                .map(|v| Secret::new(v.into()))
                .ok_or(KeyError::Malformed)
        }
    }

    #[test]
    fn key_loads_accepts_a_pem_and_refuses_anything_else_without_echoing_it() {
        let ok = key_loads(&FakeKey(Some(
            "-----BEGIN RSA PRIVATE KEY-----
QUJD
-----END RSA PRIVATE KEY-----
",
        )));
        assert!(
            ok.as_ref()
                .is_ok_and(|s| s.contains("PEM private-key header")),
            "{ok:?}"
        );
        // Known-bad controls: a non-PEM secret, and a provider that cannot decrypt.
        let bad = key_loads(&FakeKey(Some("hunter2-not-a-key")));
        assert!(
            bad.as_ref().is_err_and(|e| !e.contains("hunter2")),
            "{bad:?}"
        );
        assert!(key_loads(&FakeKey(None)).is_err());
    }
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
    fn a_sandbox_group_stops_check_and_start_naming_the_group_and_the_ruling() {
        let c = cfg("windows-sandbox", "C:/k.pem");
        let r = check(&c, true);
        assert!(
            r.problems
                .iter()
                .any(|p| p.contains("`g`") && p.contains("VM"))
        );
        let e = router(&c).err().expect("refused");
        assert!(e.contains("`g`") && e.contains("VM"), "{e}");
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

    fn egress_cfg(extra: &str) -> AdapterConfig {
        super::super::config_file::load(
            &format!(
                "org = \"O\"
app_id = 1
installation_id = 2
key_path = 'k'
job_timeout_s = 600

[[group]]
name = \"g\"
repos = [\"O/a\"]
labels = [\"antspawn\"]
launcher = \"windows-sandbox\"
{extra}"
            ),
            "t.toml",
        )
        .unwrap()
    }

    #[test]
    fn egress_extras_need_the_approvals_file_beside_the_config() {
        let dir = std::env::temp_dir().join(format!("antspawn-egress-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("adapter.toml");
        let approvals = dir.join("egress-approved.toml");
        let _ = std::fs::remove_file(&approvals);
        assert_eq!(approvals_path(&conf), approvals);
        // no extras: no file needed
        assert_eq!(load_egress(&conf, &egress_cfg("")).unwrap().len(), 1);
        // an extra with no file is refused (the known-bad control) ...
        let c = egress_cfg(
            "egress = [\"files.example.org:443\"]
",
        );
        let e = load_egress(&conf, &c).unwrap_err();
        assert!(e.contains("not approved"), "{e}");
        // ... and passes once the file approves it for the group's repo
        std::fs::write(
            &approvals,
            "[[approved]]
host = \"files.example.org\"
repo = \"O/a\"
approved = \"board 1 / 2026-11-21\"
reason = \"test\"
",
        )
        .unwrap();
        let g = load_egress(&conf, &c).unwrap();
        assert!(g[0].policy.allows("files.example.org"));
        assert!(crate::adapter::egress_policy::describe(&g[0]).contains("files.example.org:443"));
        // a malformed file is an error naming it
        std::fs::write(
            &approvals,
            "[[approved]]
host = 1
",
        )
        .unwrap();
        assert!(
            load_egress(&conf, &c)
                .unwrap_err()
                .contains("egress-approved.toml")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
