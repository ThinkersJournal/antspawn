// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the `account` launcher and the `spawn-runner` service share: the on-disk layout, the
//! runner's config, the rules a spec must satisfy to be run, and the result file format.
//!
//! The runner runs AS a restricted account that has no access to the launcher user's files, so
//! nothing is bind-mounted: `ro` sources are copied IN under `spool/<id>/in/<n>`, the runner
//! copies them into its work dir, and declared outputs are copied OUT to `results/<id>/out`.
//! Everything crosses through two directories under one root (default `%ProgramData%\antspawn`):
//!
//! ```text
//! runner.toml          admin-owned, read-only to everyone else: the ceilings
//! spool/<id>/          written by the launcher user only: spec.toml, in/<n>
//! results/<id>/        written by the runner: out/<name>, result.tmp, then exit.code last
//! work/<id>/           the runner's private work dir, wiped before and after each job
//! work/<id>.tmp/       a session's TEMP/TMP, wiped with its work dir
//! ```

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::launcher::Exit;
use crate::spec::{JobSpec, MountMode, Network};

/// The scheduled task / service the launcher triggers.
pub const TASK_NAME: &str = "AntSpawnRunner";

/// The on-disk layout under one root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Layout { root: root.into() }
    }

    /// `ANTSPAWN_ROOT`, else `%ProgramData%\antspawn`.
    pub fn from_env() -> Self {
        if let Some(r) = std::env::var_os("ANTSPAWN_ROOT") {
            return Layout::new(r);
        }
        let base = std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into());
        Layout::new(PathBuf::from(base).join("antspawn"))
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("runner.toml")
    }
    pub fn spool(&self) -> PathBuf {
        self.root.join("spool")
    }
    pub fn results(&self) -> PathBuf {
        self.root.join("results")
    }
    pub fn work(&self) -> PathBuf {
        self.root.join("work")
    }
    pub fn job_spool(&self, id: &str) -> PathBuf {
        self.spool().join(id)
    }
    pub fn job_results(&self, id: &str) -> PathBuf {
        self.results().join(id)
    }
    pub fn job_work(&self, id: &str) -> PathBuf {
        self.work().join(id)
    }
    /// A session's `TEMP`/`TMP`: beside its work dir, wiped with it (`valid_job_id` has no `.`, so
    /// this can never be another job's work dir).
    pub fn job_scratch(&self, id: &str) -> PathBuf {
        self.work().join(format!("{id}.tmp"))
    }
}

/// Remove the spool and results entries of **sessions** that ended (`exit.code` published) at least
/// `min_age` ago and that nobody collected: their owner died before it could. Returns the ids
/// removed. A job's entry (`spec.toml`) is never touched here, a session that has not ended has no
/// `exit.code`, and a recent one is left to its owner, whose `close` is still reading it.
pub fn reap_ended_sessions(layout: &Layout, min_age: std::time::Duration) -> Vec<String> {
    let mut reaped = Vec::new();
    let Ok(entries) = std::fs::read_dir(layout.spool()) else {
        return reaped;
    };
    for id in entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|id| valid_job_id(id))
    {
        if !layout.job_spool(&id).join("session.toml").is_file() {
            continue;
        }
        let ended = std::fs::metadata(layout.job_results(&id).join("exit.code"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok());
        if !ended.is_some_and(|age| age >= min_age) {
            continue;
        }
        let spool = std::fs::remove_dir_all(layout.job_spool(&id));
        let results = std::fs::remove_dir_all(layout.job_results(&id));
        if spool.is_ok() && results.is_ok() {
            reaped.push(id);
        }
    }
    reaped.sort();
    reaped
}

/// A job id is a directory name the runner will join onto paths: letters, digits and `-` only.
pub fn valid_job_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The runner's ceilings. Written once by the elevated setup; the runner account cannot change it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerConfig {
    /// Longest time limit a job may ask for, and the limit used when it asks for none.
    pub max_time_s: u64,
    /// Largest per-process memory limit a job may ask for.
    pub max_ram_mb: u64,
}

impl RunnerConfig {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| format!("runner.toml: {e}"))
    }

    /// The time limit that applies to `spec`.
    pub fn time_limit_s(&self, spec: &JobSpec) -> u64 {
        spec.limits.time_s.unwrap_or(self.max_time_s)
    }
}

/// A path the job names (cwd, mount, output): relative, no `..`, no drive or root. Under copy-in
/// semantics it is a place inside the work dir, never a host path.
pub fn safe_relative(p: &str) -> bool {
    !p.is_empty()
        && !p.contains(':')
        && !p.starts_with(['/', '\\'])
        && Path::new(p)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Refuse a spec the `account` launcher cannot honour or the runner's config does not allow.
/// The launcher calls this before spooling and the runner calls it again before running, so
/// neither trusts the other.
pub fn check_spec(spec: &JobSpec, cfg: &RunnerConfig) -> Result<(), String> {
    if spec.limits.cpu.is_some() {
        return Err("limits.cpu is not enforced by the account launcher; refusing".into());
    }
    if spec
        .limits
        .time_s
        .is_some_and(|t| t == 0 || t > cfg.max_time_s)
    {
        return Err(format!(
            "limits.time_s must be 1..={} for this runner",
            cfg.max_time_s
        ));
    }
    if spec
        .limits
        .ram_mb
        .is_some_and(|r| r == 0 || r > cfg.max_ram_mb)
    {
        return Err(format!(
            "limits.ram_mb must be 1..={} for this runner",
            cfg.max_ram_mb
        ));
    }
    if spec.needs.network == Network::On {
        return Err("needs.network = \"on\" is not provided by the account launcher".into());
    }
    if !matches!(spec.needs.gpu, crate::spec::Gpu::None) {
        return Err("gpu is not provided by the account launcher".into());
    }
    for m in &spec.mounts {
        if m.mode == MountMode::Rw {
            return Err(format!(
                "mount `{}`: rw is impossible under copy-in/copy-out; use ro plus an [[output]]",
                m.guest
            ));
        }
        if !safe_relative(&m.guest) {
            return Err(format!(
                "mount `{}`: guest paths must be relative to the work dir",
                m.guest
            ));
        }
    }
    if let Some(c) = &spec.job.cwd
        && !safe_relative(c)
    {
        return Err(format!("job.cwd `{c}` must be relative to the work dir"));
    }
    for o in &spec.outputs {
        if !safe_relative(&o.guest) {
            return Err(format!(
                "output `{}`: guest paths must be relative to the work dir",
                o.guest
            ));
        }
    }
    Ok(())
}

/// Copy a file or a directory tree. Symlinks are followed, so a link cannot smuggle a path out.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to)?;
    }
    Ok(())
}

/// The text of `exit.code`.
pub fn encode_exit(e: Exit) -> String {
    match e {
        Exit::Code(c) => c.to_string(),
        Exit::TimedOut => "timeout".into(),
        Exit::Killed => "killed".into(),
    }
}

/// Parse `exit.code`; `None` for anything unrecognised (never guessed into a success).
pub fn decode_exit(text: &str) -> Option<Exit> {
    match text.trim() {
        "timeout" => Some(Exit::TimedOut),
        "killed" => Some(Exit::Killed),
        n => n.parse().ok().map(Exit::Code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: RunnerConfig = RunnerConfig {
        max_time_s: 600,
        max_ram_mb: 4096,
    };

    fn spec(extra: &str) -> JobSpec {
        JobSpec::from_toml(&format!("[job]\nname = \"t\"\ncmd = [\"x\"]\n{extra}")).unwrap()
    }

    fn temp_layout(tag: &str) -> Layout {
        let d = std::env::temp_dir().join(format!("antspawn-reap-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Layout::new(d)
    }

    /// Make `spool/<id>/<marker>` and, when `ended_ago` is given, `results/<id>/exit.code` of that age.
    fn entry(l: &Layout, id: &str, marker: &str, ended_ago: Option<std::time::Duration>) {
        std::fs::create_dir_all(l.job_spool(id)).unwrap();
        std::fs::write(l.job_spool(id).join(marker), "x").unwrap();
        if let Some(ago) = ended_ago {
            std::fs::create_dir_all(l.job_results(id)).unwrap();
            let f = std::fs::File::create(l.job_results(id).join("exit.code")).unwrap();
            f.set_modified(std::time::SystemTime::now() - ago).unwrap();
        }
    }

    #[test]
    fn only_old_ended_sessions_are_reaped() {
        use std::time::Duration;
        let l = temp_layout("reap");
        let old = Some(Duration::from_secs(3600));
        entry(&l, "s-old", "session.toml", old);
        entry(&l, "s-young", "session.toml", Some(Duration::from_secs(1)));
        entry(&l, "s-running", "session.toml", None);
        entry(&l, "job-old", "spec.toml", old);
        let min = Duration::from_secs(60);
        assert_eq!(reap_ended_sessions(&l, min), ["s-old"]);
        assert!(!l.job_spool("s-old").exists() && !l.job_results("s-old").exists());
        for kept in ["s-young", "s-running", "job-old"] {
            assert!(l.job_spool(kept).exists(), "{kept} must stay");
        }
        // Control: the same call reaps the others once they qualify (age is the only difference).
        assert_eq!(reap_ended_sessions(&l, Duration::ZERO), ["s-young"]);
        assert!(l.job_spool("s-running").exists() && l.job_spool("job-old").exists());
        // A second pass finds nothing left to reap.
        assert!(reap_ended_sessions(&l, Duration::ZERO).is_empty());
        let _ = std::fs::remove_dir_all(&l.root);
    }

    #[test]
    fn a_session_scratch_dir_is_never_another_jobs_work_dir() {
        let l = Layout::new("R");
        assert_ne!(l.job_scratch("a"), l.job_work("a"));
        assert!(
            !valid_job_id("a.tmp"),
            "so no job id can name a scratch dir"
        );
    }

    #[test]
    fn job_ids_cannot_climb() {
        assert!(valid_job_id("a1-b2"));
        for bad in ["", "..", "a/b", "a\\b", "a b", "a.b"] {
            assert!(!valid_job_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn guest_paths_must_stay_inside_the_work_dir() {
        for ok in ["src", "a/b", "./a", "a\\b"] {
            assert!(safe_relative(ok), "{ok}");
        }
        for bad in ["", "..", "a/../b", "/etc", "\\x", "C:\\x", "C:x"] {
            assert!(!safe_relative(bad), "{bad}");
        }
    }

    #[test]
    fn limits_above_the_config_are_refused() {
        assert!(check_spec(&spec(""), &CFG).is_ok());
        assert!(check_spec(&spec("[limits]\ntime_s = 601\n"), &CFG).is_err());
        assert!(check_spec(&spec("[limits]\ntime_s = 600\nram_mb = 4096\n"), &CFG).is_ok());
        assert!(check_spec(&spec("[limits]\nram_mb = 4097\n"), &CFG).is_err());
        assert!(check_spec(&spec("[limits]\ncpu = 2\n"), &CFG).is_err());
    }

    #[test]
    fn unprovided_things_are_refused_not_ignored() {
        assert!(check_spec(&spec("[needs]\nnetwork = \"on\"\n"), &CFG).is_err());
        assert!(check_spec(&spec("[needs]\ngpu = \"cuda\"\n"), &CFG).is_err());
        let rw = "[[mount]]\nhost = \"C:/src\"\nguest = \"s\"\nmode = \"rw\"\n";
        assert!(check_spec(&spec(rw), &CFG).is_err());
        let abs = "[[mount]]\nhost = \"C:/src\"\nguest = \"C:\\\\s\"\nmode = \"ro\"\n";
        assert!(check_spec(&spec(abs), &CFG).is_err());
        assert!(check_spec(&spec("[[output]]\nguest = \"../x\"\nname = \"o\"\n"), &CFG).is_err());
    }

    #[test]
    fn exit_codes_round_trip_and_garbage_is_none() {
        for e in [Exit::Code(0), Exit::Code(-3), Exit::TimedOut, Exit::Killed] {
            assert_eq!(decode_exit(&encode_exit(e)), Some(e));
        }
        assert_eq!(decode_exit("ok"), None);
        assert_eq!(decode_exit(""), None);
    }

    #[test]
    fn config_rejects_unknown_keys() {
        assert!(RunnerConfig::from_toml("max_time_s = 1\nmax_ram_mb = 1\n").is_ok());
        assert!(RunnerConfig::from_toml("max_time_s = 1\nmax_ram_mb = 1\nx = 2\n").is_err());
    }
}
