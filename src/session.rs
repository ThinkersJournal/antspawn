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
use crate::streams::Streams;

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
