// SPDX-License-Identifier: MIT OR Apache-2.0
//! GitHub adapter types: a GitHub App mints one just-in-time runner config per queued job
//! (`generate-jitconfig`, ephemeral). Design: `docs/ADAPTER-GITHUB.md`.
//!
//! Nothing here talks to the network or signs anything. The real HTTP client implements
//! [`GithubApi`]; the App's private key reaches it only through a [`KeyProvider`], so tests use fakes.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

/// Longest job time the adapter will grant, whatever a spec asks for (6 hours).
pub const MAX_JOB_TIME_S: u64 = 6 * 3600;

/// A credential (private key, JIT config). Debug is redacted; there is no Display, Clone or
/// Serialize, so it cannot reach a log, an env block or a file by accident.
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Secret(value)
    }
    /// The only way to read it; call sites are the signer and the runner hand-off.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Why the App key could not be provided. Messages never contain key material.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("key path `{}` must be absolute", .0.display())]
    Relative(PathBuf),
    #[error("key path `{}` is inside the job sandbox (`{}`); the key must live outside it", .key.display(), .root.display())]
    InsideSandbox { key: PathBuf, root: PathBuf },
    #[error("key file `{}` is absent or unreadable", .0.display())]
    Unreadable(PathBuf),
    #[error("the App key is not a usable RSA private key")]
    Malformed,
}

/// Supplies the GitHub App private key.
pub trait KeyProvider {
    fn load(&self) -> Result<Secret, KeyError>;
}

/// Lexically normalised, lower-cased components (`.` and `..` resolved), so `C:\a\..\B` and
/// `c:\b` compare equal. Conservative: case folding can only refuse more.
fn normal(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str().to_string_lossy().to_lowercase()),
        }
    }
    out
}

fn is_inside(path: &Path, root: &Path) -> bool {
    let (p, r) = (normal(path), normal(root));
    !r.is_empty() && p.starts_with(&r)
}

/// Refuse a key path that is relative or lies under any sandbox root (a mount's host path, the
/// work dir, the spool or results dirs). Checks the path as written and, when it exists, as
/// resolved, so a link into the sandbox is caught too.
pub fn check_key_path(key: &Path, sandbox_roots: &[PathBuf]) -> Result<(), KeyError> {
    if !key.is_absolute() {
        return Err(KeyError::Relative(key.to_path_buf()));
    }
    let resolved = std::fs::canonicalize(key).ok();
    for root in sandbox_roots {
        let root_resolved = std::fs::canonicalize(root).ok();
        let hit = is_inside(key, root)
            || resolved.as_deref().is_some_and(|r| is_inside(r, root))
            || root_resolved.as_deref().is_some_and(|r| is_inside(key, r))
            || matches!((&resolved, &root_resolved), (Some(a), Some(b)) if is_inside(a, b));
        if hit {
            return Err(KeyError::InsideSandbox {
                key: key.to_path_buf(),
                root: root.clone(),
            });
        }
    }
    Ok(())
}

/// Reads the key from a file under the watcher account. The path is checked on construction and
/// again on every load.
pub struct FileKeyProvider {
    path: PathBuf,
    sandbox_roots: Vec<PathBuf>,
}

impl FileKeyProvider {
    pub fn new(path: PathBuf, sandbox_roots: Vec<PathBuf>) -> Result<Self, KeyError> {
        check_key_path(&path, &sandbox_roots)?;
        Ok(FileKeyProvider {
            path,
            sandbox_roots,
        })
    }
}

impl KeyProvider for FileKeyProvider {
    fn load(&self) -> Result<Secret, KeyError> {
        check_key_path(&self.path, &self.sandbox_roots)?;
        std::fs::read_to_string(&self.path)
            .map(Secret::new)
            .map_err(|_| KeyError::Unreadable(self.path.clone()))
    }
}

/// One runner group: the repos allowed to use it and the labels that route a job to it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerGroup {
    pub name: String,
    /// `owner/name`, exact; no wildcards.
    pub repos: Vec<String>,
    /// A workflow's `runs-on` must carry all of these.
    pub labels: Vec<String>,
    /// The AntSpawn launcher id that runs this group's jobs.
    pub launcher: String,
    /// `account` launcher only: the scheduled task that runs the shim as the runner account.
    #[serde(default)]
    pub task: Option<String>,
    /// `account` launcher only: the pipe name that task's shim creates (a fixed argument of the task).
    #[serde(default)]
    pub pipe: Option<String>,
}

/// The adapter's configuration (TOML, strict). Holds a key *path*, never a key.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterConfig {
    pub org: String,
    pub app_id: u64,
    pub installation_id: u64,
    pub key_path: PathBuf,
    /// Ceiling for every job in seconds; a spec may ask for less.
    pub job_timeout_s: u64,
    #[serde(rename = "group")]
    pub groups: Vec<RunnerGroup>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Parse(String),
    #[error("invalid config: {0}")]
    Invalid(String),
}

fn invalid<T>(msg: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Invalid(msg.into()))
}

impl AdapterConfig {
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let cfg: AdapterConfig =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.org.trim().is_empty() {
            return invalid("org is empty");
        }
        if self.job_timeout_s == 0 || self.job_timeout_s > MAX_JOB_TIME_S {
            return invalid(format!("job_timeout_s must be 1..={MAX_JOB_TIME_S}"));
        }
        if self.groups.is_empty() {
            return invalid("no [[group]] declared");
        }
        let (mut names, mut label_sets) = (BTreeSet::new(), BTreeSet::new());
        let (mut tasks, mut pipes) = (BTreeSet::new(), BTreeSet::new());
        for g in &self.groups {
            if g.name.trim().is_empty() || !names.insert(g.name.as_str()) {
                return invalid(format!("group name `{}` is empty or repeated", g.name));
            }
            if g.launcher.trim().is_empty() {
                return invalid(format!("group `{}` names no launcher", g.name));
            }
            let blank = |o: &Option<String>| o.as_deref().is_none_or(|s| s.trim().is_empty());
            if g.launcher == "account" {
                if blank(&g.task) || blank(&g.pipe) {
                    return invalid(format!(
                        "group `{}` uses the account launcher and needs `task` and `pipe`",
                        g.name
                    ));
                }
                let (t, p) = (
                    g.task.as_deref().unwrap_or(""),
                    g.pipe.as_deref().unwrap_or(""),
                );
                if !tasks.insert(t) || !pipes.insert(p) {
                    return invalid(format!(
                        "group `{}` repeats another group's task or pipe",
                        g.name
                    ));
                }
            } else if g.task.is_some() || g.pipe.is_some() {
                return invalid(format!(
                    "group `{}` sets `task` or `pipe`, which only the account launcher uses",
                    g.name
                ));
            }
            if g.labels.is_empty() || g.labels.iter().any(|l| l.trim().is_empty()) {
                return invalid(format!("group `{}` needs non-empty labels", g.name));
            }
            // Two groups with one label set would route a job ambiguously.
            let set: BTreeSet<&str> = g.labels.iter().map(String::as_str).collect();
            if !label_sets.insert(set) {
                return invalid(format!(
                    "group `{}` repeats another group's label set",
                    g.name
                ));
            }
            if g.repos.is_empty() {
                return invalid(format!("group `{}` names no repos", g.name));
            }
            for r in &g.repos {
                let ok = r.split_once('/').is_some_and(|(o, n)| {
                    o == self.org && !n.is_empty() && !n.contains('/') && !r.contains('*')
                });
                if !ok {
                    return invalid(format!(
                        "repo `{r}` must be exactly `{}/<name>`, no wildcard",
                        self.org
                    ));
                }
            }
        }
        Ok(())
    }

    /// The group whose labels are all carried by `runs_on`, if exactly one.
    pub fn route(&self, repo: &str, runs_on: &[String]) -> Option<&RunnerGroup> {
        let mut hits = self.groups.iter().filter(|g| {
            g.repos.iter().any(|r| r == repo) && g.labels.iter().all(|l| runs_on.contains(l))
        });
        match (hits.next(), hits.next()) {
            (Some(g), None) => Some(g),
            _ => None,
        }
    }

    /// Job time granted: the spec's request, never above the configured ceiling.
    pub fn clamp_time(&self, requested: Option<u64>) -> u64 {
        requested.map_or(self.job_timeout_s, |t| t.clamp(1, self.job_timeout_s))
    }
}

/// How GitHub reports an org runner group (the fields the adapter checks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInfo {
    /// `"all"`, `"private"` or `"selected"`.
    pub visibility: String,
    /// `owner/name` of every repo the group is open to (when `selected`).
    pub repos: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum GroupError {
    #[error("group `{group}` visibility is `{found}`, must be `selected`")]
    NotSelected { group: String, found: String },
    #[error("group `{group}` is open to repos not in the config: {}", .extra.join(", "))]
    ExtraRepos { group: String, extra: Vec<String> },
    #[error("group `{group}` is missing configured repos: {}", .missing.join(", "))]
    MissingRepos { group: String, missing: Vec<String> },
}

/// Check the live group against the config before minting for it: restricted to exactly the
/// named repos (extra repos refused, absent ones reported).
pub fn verify_group(info: &GroupInfo, want: &RunnerGroup) -> Result<(), GroupError> {
    let group = want.name.clone();
    if info.visibility != "selected" {
        return Err(GroupError::NotSelected {
            group,
            found: info.visibility.clone(),
        });
    }
    let have: BTreeSet<&String> = info.repos.iter().collect();
    let need: BTreeSet<&String> = want.repos.iter().collect();
    let extra: Vec<String> = have.difference(&need).map(|s| s.to_string()).collect();
    if !extra.is_empty() {
        return Err(GroupError::ExtraRepos { group, extra });
    }
    let missing: Vec<String> = need.difference(&have).map(|s| s.to_string()).collect();
    if !missing.is_empty() {
        return Err(GroupError::MissingRepos { group, missing });
    }
    Ok(())
}

/// What to ask GitHub for: one ephemeral runner for one queued job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JitRequest {
    pub org: String,
    pub group: String,
    /// Unique per job, e.g. `antspawn-<run>-<job>`.
    pub runner_name: String,
    pub labels: Vec<String>,
}

/// Why minting failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MintError {
    #[error("the App key is unavailable: {0}")]
    Key(#[from] KeyError),
    #[error("GitHub rejected the App credentials")]
    Auth,
    #[error("rate limited; retry after {retry_after_s}s")]
    RateLimited { retry_after_s: u64 },
    #[error("GitHub unavailable: {0}")]
    Unavailable(String),
    #[error("GitHub refused the request: {0}")]
    Rejected(String),
    #[error(transparent)]
    Group(#[from] GroupError),
}

impl MintError {
    /// Retrying cannot fix these; an operator has to.
    pub fn needs_operator(&self) -> bool {
        matches!(
            self,
            MintError::Key(_) | MintError::Auth | MintError::Rejected(_) | MintError::Group(_)
        )
    }
}

/// The GitHub REST calls the adapter needs. The real client signs an App JWT with the key from
/// its [`KeyProvider`], exchanges it for an installation token and calls the endpoints.
pub trait GithubApi {
    fn runner_group(&self, org: &str, group: &str) -> Result<GroupInfo, MintError>;
    /// `POST /orgs/{org}/actions/runners/generate-jitconfig`; the result is the encoded JIT config.
    fn generate_jitconfig(&self, req: &JitRequest) -> Result<Secret, MintError>;
}

impl<T: GithubApi + ?Sized> GithubApi for &T {
    fn runner_group(&self, org: &str, group: &str) -> Result<GroupInfo, MintError> {
        (**self).runner_group(org, group)
    }
    fn generate_jitconfig(&self, req: &JitRequest) -> Result<Secret, MintError> {
        (**self).generate_jitconfig(req)
    }
}

/// Result of one minting attempt.
#[derive(Debug)]
pub enum MintOutcome {
    Minted(Secret),
    /// No runner was minted. The GitHub job stays queued and nothing else is touched.
    StayQueued {
        reason: String,
        retry_at_s: u64,
        operator: bool,
    },
}

/// Spacing between attempts: doubles per consecutive failure from `min_gap_s` up to `max_gap_s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub min_gap_s: u64,
    pub max_gap_s: u64,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            min_gap_s: 5,
            max_gap_s: 900,
        }
    }
}

/// Mints at most one runner per call; failures push the next allowed attempt out, so a GitHub
/// outage or a bad key cannot produce a retry storm. Time is passed in, never read.
pub struct Minter<A: GithubApi> {
    api: A,
    backoff: Backoff,
    failures: u32,
    next_allowed_s: u64,
}

impl<A: GithubApi> Minter<A> {
    pub fn new(api: A, backoff: Backoff) -> Self {
        Minter {
            api,
            backoff,
            failures: 0,
            next_allowed_s: 0,
        }
    }

    pub fn mint(&mut self, now_s: u64, group: &RunnerGroup, req: &JitRequest) -> MintOutcome {
        if now_s < self.next_allowed_s {
            return MintOutcome::StayQueued {
                reason: "cooling down after a failed attempt".into(),
                retry_at_s: self.next_allowed_s,
                operator: false,
            };
        }
        let attempt = self
            .api
            .runner_group(&req.org, &group.name)
            .and_then(|info| verify_group(&info, group).map_err(MintError::from))
            .and_then(|()| self.api.generate_jitconfig(req));
        match attempt {
            Ok(cfg) => {
                self.failures = 0;
                self.next_allowed_s = 0;
                MintOutcome::Minted(cfg)
            }
            Err(e) => {
                let doubled = self
                    .backoff
                    .min_gap_s
                    .saturating_mul(1u64 << self.failures.min(20));
                let mut gap = doubled.min(self.backoff.max_gap_s);
                if e.needs_operator() {
                    gap = self.backoff.max_gap_s;
                }
                if let MintError::RateLimited { retry_after_s } = e {
                    gap = gap.max(retry_after_s);
                }
                self.failures = self.failures.saturating_add(1);
                self.next_allowed_s = now_s.saturating_add(gap);
                MintOutcome::StayQueued {
                    reason: e.to_string(),
                    retry_at_s: self.next_allowed_s,
                    operator: e.needs_operator(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const CFG: &str = r#"
org = "ThinkersJournal"
app_id = 1
installation_id = 2
key_path = "C:/ProgramData/antspawn-watcher/app.pem"
job_timeout_s = 3600

[[group]]
name = "antspawn-account"
repos = ["ThinkersJournal/antspawn"]
labels = ["antspawn", "account"]
launcher = "account"
task = "antspawn-r1"
pipe = "antspawn-r1"

[[group]]
name = "antspawn-sandbox"
repos = ["ThinkersJournal/antspawn", "ThinkersJournal/other"]
labels = ["antspawn", "windows-sandbox"]
launcher = "windows-sandbox"
"#;

    fn cfg() -> AdapterConfig {
        AdapterConfig::from_toml(CFG).unwrap()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn secret_debug_is_redacted() {
        let sec = Secret::new("-----BEGIN KEY-----".into());
        assert_eq!(format!("{sec:?}"), "Secret(<redacted>)");
    }

    #[test]
    fn config_parses_and_routes_by_labels() {
        let c = cfg();
        let g = c.route(
            "ThinkersJournal/antspawn",
            &s(&["self-hosted", "antspawn", "account"]),
        );
        assert_eq!(g.map(|g| g.launcher.as_str()), Some("account"));
        // Unlisted repo, or labels not all carried: no route.
        assert!(
            c.route("ThinkersJournal/other", &s(&["antspawn", "account"]))
                .is_none()
        );
        assert!(
            c.route("ThinkersJournal/antspawn", &s(&["antspawn"]))
                .is_none()
        );
    }

    #[test]
    fn config_rejects_bad_shapes() {
        for (from, to) in [
            (
                "repos = [\"ThinkersJournal/antspawn\"]",
                "repos = [\"ThinkersJournal/*\"]",
            ),
            ("job_timeout_s = 3600", "job_timeout_s = 0"),
            ("job_timeout_s = 3600", "job_timeout_s = 999999"),
            (
                "labels = [\"antspawn\", \"windows-sandbox\"]",
                "labels = [\"account\", \"antspawn\"]",
            ),
            (
                "repos = [\"ThinkersJournal/antspawn\"]",
                "repos = [\"Elsewhere/x\"]",
            ),
            (
                "launcher = \"account\"",
                "launcher = \"account\"\nsecret = \"x\"",
            ),
        ] {
            let text = CFG.replacen(from, to, 1);
            assert_ne!(text, CFG, "mutation did not apply: {to}");
            assert!(AdapterConfig::from_toml(&text).is_err(), "accepted: {to}");
        }
    }

    #[test]
    fn time_is_clamped_to_the_ceiling() {
        let c = cfg();
        assert_eq!(
            (
                c.clamp_time(None),
                c.clamp_time(Some(60)),
                c.clamp_time(Some(99999))
            ),
            (3600, 60, 3600)
        );
    }

    #[test]
    fn key_inside_the_sandbox_is_refused() {
        let root = std::env::temp_dir().join("antspawn-sbx");
        let inside = root.join("work").join("app.pem");
        let dotdot = root.join("elsewhere").join("..").join("app.pem");
        for key in [&inside, &dotdot] {
            assert!(matches!(
                check_key_path(key, std::slice::from_ref(&root)),
                Err(KeyError::InsideSandbox { .. })
            ));
            assert!(FileKeyProvider::new(key.clone(), vec![root.clone()]).is_err());
        }
        // Positive control: outside the root is accepted, relative is not.
        let outside = std::env::temp_dir()
            .join("antspawn-watcher")
            .join("app.pem");
        assert_eq!(
            check_key_path(&outside, std::slice::from_ref(&root)),
            Ok(())
        );
        assert!(matches!(
            check_key_path(Path::new("app.pem"), &[]),
            Err(KeyError::Relative(_))
        ));
    }

    #[test]
    fn key_load_never_echoes_contents_and_rechecks() {
        let dir = std::env::temp_dir().join(format!("antspawn-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("app.pem");
        std::fs::write(&key, "TOPSECRET").unwrap();
        let p = FileKeyProvider::new(key.clone(), vec![]).unwrap();
        assert_eq!(p.load().unwrap().expose(), "TOPSECRET");
        std::fs::remove_file(&key).unwrap();
        let err = p.load().unwrap_err();
        assert!(matches!(err, KeyError::Unreadable(_)));
        assert!(!err.to_string().contains("TOPSECRET"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn group_must_be_selected_and_exactly_the_named_repos() {
        let c = cfg();
        let g = &c.groups[0];
        let sel = |repos: &[&str]| GroupInfo {
            visibility: "selected".into(),
            repos: s(repos),
        };
        assert_eq!(verify_group(&sel(&["ThinkersJournal/antspawn"]), g), Ok(()));
        let all = GroupInfo {
            visibility: "all".into(),
            repos: vec![],
        };
        assert!(matches!(
            verify_group(&all, g),
            Err(GroupError::NotSelected { .. })
        ));
        let extra = sel(&["ThinkersJournal/antspawn", "ThinkersJournal/x"]);
        assert!(matches!(
            verify_group(&extra, g),
            Err(GroupError::ExtraRepos { .. })
        ));
        assert!(matches!(
            verify_group(&sel(&[]), g),
            Err(GroupError::MissingRepos { .. })
        ));
    }

    struct Fake {
        group: GroupInfo,
        errs: RefCell<Vec<MintError>>,
        calls: Cell<u32>,
    }

    impl GithubApi for Fake {
        fn runner_group(&self, _: &str, _: &str) -> Result<GroupInfo, MintError> {
            Ok(self.group.clone())
        }
        fn generate_jitconfig(&self, _: &JitRequest) -> Result<Secret, MintError> {
            self.calls.set(self.calls.get() + 1);
            match self.errs.borrow_mut().pop() {
                Some(e) => Err(e),
                None => Ok(Secret::new("jit".into())),
            }
        }
    }

    fn fake(errs: Vec<MintError>) -> Fake {
        Fake {
            group: GroupInfo {
                visibility: "selected".into(),
                repos: s(&["ThinkersJournal/antspawn"]),
            },
            errs: RefCell::new(errs),
            calls: Cell::new(0),
        }
    }

    fn req(c: &AdapterConfig) -> JitRequest {
        JitRequest {
            org: c.org.clone(),
            group: c.groups[0].name.clone(),
            runner_name: "antspawn-1-1".into(),
            labels: c.groups[0].labels.clone(),
        }
    }

    #[test]
    fn failed_mint_leaves_the_job_queued_and_backs_off_without_a_storm() {
        let c = cfg();
        let (g, r) = (&c.groups[0], req(&c));
        let errs = vec![
            MintError::Unavailable("503".into()),
            MintError::Unavailable("503".into()),
        ];
        let mut m = Minter::new(
            fake(errs),
            Backoff {
                min_gap_s: 5,
                max_gap_s: 60,
            },
        );
        // First failure: retry in 5 s. Calls inside the window do not reach the API.
        let MintOutcome::StayQueued {
            retry_at_s,
            operator,
            ..
        } = m.mint(100, g, &r)
        else {
            panic!("expected StayQueued")
        };
        assert_eq!((retry_at_s, operator), (105, false));
        for t in 101..105 {
            assert!(matches!(
                m.mint(t, g, &r),
                MintOutcome::StayQueued {
                    retry_at_s: 105,
                    ..
                }
            ));
        }
        assert_eq!(m.api.calls.get(), 1);
        // Second failure doubles the gap; then success resets it.
        let MintOutcome::StayQueued { retry_at_s, .. } = m.mint(105, g, &r) else {
            panic!("expected StayQueued")
        };
        assert_eq!(retry_at_s, 115);
        assert!(matches!(m.mint(115, g, &r), MintOutcome::Minted(_)));
        assert_eq!(m.failures, 0);
    }

    #[test]
    fn operator_errors_wait_the_longest_and_rate_limits_are_honoured() {
        let c = cfg();
        let (g, r) = (&c.groups[0], req(&c));
        let b = Backoff {
            min_gap_s: 5,
            max_gap_s: 60,
        };
        let mut m = Minter::new(fake(vec![MintError::Auth]), b);
        let MintOutcome::StayQueued {
            retry_at_s,
            operator,
            ..
        } = m.mint(0, g, &r)
        else {
            panic!("expected StayQueued")
        };
        assert_eq!((retry_at_s, operator), (60, true));
        let mut m = Minter::new(fake(vec![MintError::RateLimited { retry_after_s: 300 }]), b);
        let MintOutcome::StayQueued { retry_at_s, .. } = m.mint(0, g, &r) else {
            panic!("expected StayQueued")
        };
        assert_eq!(retry_at_s, 300);
    }

    #[test]
    fn an_unrestricted_group_is_never_minted_for() {
        let c = cfg();
        let (g, r) = (&c.groups[0], req(&c));
        let mut api = fake(vec![]);
        api.group.visibility = "all".into();
        let mut m = Minter::new(api, Backoff::default());
        assert!(matches!(
            m.mint(0, g, &r),
            MintOutcome::StayQueued { operator: true, .. }
        ));
        assert_eq!(m.api.calls.get(), 0);
    }
}
