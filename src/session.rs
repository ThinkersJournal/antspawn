// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sessions: one environment that lives across many `exec`s (agent-loop ask A1).
//!
//! A job is provision, run once, collect, destroy ([`crate::Environment`]). A session is open, many
//! `exec`s that share state, close. State persisting between execs is the point, so the capability
//! `clean-slate-next-run` does not describe it: `session` says the launcher can host one, and
//! `clean-slate-next-session` says nothing outlives `close`. Both are off until the matrix says
//! `yes`; [`crate::Registry::open_session`] refuses with the unmet need named and never falls back
//! to a weaker launcher.
//!
//! This module holds the launcher-independent parts; `docs/SESSION-PLAN.md` has the slice order.

use std::collections::BTreeMap;

use crate::launcher::{Exit, LaunchError};
use crate::runner::safe_relative;
use crate::session_proto::FileInfo;
use crate::streams::Streams;
use crate::workspace::{ApplyReport, Approvals};
use crate::workspace_session::StagedSummary;

/// One command to run inside a session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecRequest {
    pub cmd: Vec<String>,
    /// Relative to the session's workspace; absolute, drive and `..` paths are refused.
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    /// Limit for this exec only (not for the session).
    pub time_s: Option<u64>,
}

impl ExecRequest {
    pub fn new<I, S>(cmd: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ExecRequest {
            cmd: cmd.into_iter().map(Into::into).collect(),
            ..ExecRequest::default()
        }
    }

    /// Refuse a request no launcher should attempt.
    pub fn validate(&self) -> Result<(), String> {
        if self.cmd.first().is_none_or(String::is_empty) {
            return Err("exec: cmd must name a program".into());
        }
        if self.time_s == Some(0) {
            return Err("exec: time_s must be at least 1".into());
        }
        if let Some(c) = &self.cwd
            && !safe_relative(c)
        {
            return Err(format!("exec: cwd `{c}` must be relative to the workspace"));
        }
        if self.env.keys().any(|k| k.is_empty() || k.contains('=')) {
            return Err("exec: env names must be non-empty and contain no `=`".into());
        }
        Ok(())
    }
}

/// How one exec ended and what it printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    pub exit: Exit,
    pub streams: Streams,
}

/// A live session. `close` must also happen on `Drop` (panic, ctrl-c).
pub trait Session {
    /// Run `req` to completion (or until its time limit). State from earlier execs is visible.
    fn exec(&mut self, req: &ExecRequest) -> Result<ExecOutcome, LaunchError>;
    /// End the session and everything in it.
    fn close(self: Box<Self>) -> Result<(), LaunchError>;

    /// Write one chunk of `path` in the session's workspace (slice W5): offset 0 creates or
    /// truncates, any other offset must equal the file's length now. Returns the length after.
    fn put(&mut self, _path: &str, _offset: u64, _data: &[u8]) -> Result<u64, LaunchError> {
        Err(no_files())
    }
    /// Read up to `len` bytes of `path` from `offset`; `true` when that reached the end of the file.
    fn get(
        &mut self,
        _path: &str,
        _offset: u64,
        _len: u64,
    ) -> Result<(Vec<u8>, bool), LaunchError> {
        Err(no_files())
    }
    /// Regular files in the workspace with size and SHA-256, sorted, after `after`; `true` when
    /// another page follows.
    fn list(&mut self, _after: Option<&str>) -> Result<(Vec<FileInfo>, bool), LaunchError> {
        Err(no_files())
    }

    /// `copy-back` only: fetch what the session changed against its base and keep it staged for
    /// [`Session::apply_staged`]. Replaces any earlier staging.
    fn stage_changes(&mut self) -> Result<StagedSummary, LaunchError> {
        Err(no_files())
    }
    /// `copy-back` only: write the staged change set into the real project, as the invoking user,
    /// all or nothing. `set_hash` must equal the staged set's hash, so what a person reviewed is
    /// what lands. `ok` is the harness's approval by exact path; this crate holds none.
    fn apply_staged(
        &mut self,
        _set_hash: &str,
        _ok: &Approvals,
    ) -> Result<ApplyReport, LaunchError> {
        Err(no_files())
    }
}

fn no_files() -> LaunchError {
    LaunchError::Other("this session does not provide workspace file operations".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_must_name_a_program_and_stay_in_the_workspace() {
        assert!(ExecRequest::new(["x"]).validate().is_ok());
        assert!(ExecRequest::new(Vec::<String>::new()).validate().is_err());
        assert!(ExecRequest::new([""]).validate().is_err());
        let mut r = ExecRequest::new(["x"]);
        for ok in ["src", "a/b"] {
            r.cwd = Some(ok.into());
            assert!(r.validate().is_ok(), "{ok}");
        }
        for bad in ["..", "/etc", "C:\\x", "a/../b"] {
            r.cwd = Some(bad.into());
            assert!(r.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn zero_time_and_bad_env_names_are_refused() {
        let mut r = ExecRequest::new(["x"]);
        r.time_s = Some(0);
        assert!(r.validate().is_err());
        r.time_s = Some(1);
        assert!(r.validate().is_ok());
        for bad in ["", "A=B"] {
            r.env.insert(bad.into(), "v".into());
            assert!(r.validate().is_err(), "{bad:?}");
            r.env.clear();
        }
    }
}
