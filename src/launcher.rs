// SPDX-License-Identifier: MIT OR Apache-2.0
//! The launcher trait and the provider selection built on the matrix.
//!
//! Flow (same shape as PersistAnt): [`Registry::providers`] lists launchers that satisfy ALL the
//! needs, in the spec's order of importance, naming each unmet need (no closest match); the
//! operator stores one id; [`Registry::open_named`] re-checks the matrix at run time and refuses
//! a launcher that no longer satisfies the needs. Launchers never elevate and never create
//! accounts: whatever they need must already be provisioned, and they say so loudly if it is not.
//! Ranking and the rule for when a recorded `yes` counts are CapAnt's.

use std::path::Path;

use crate::capability::Capability;
use crate::matrix::Matrix;
use crate::spec::{JobSpec, Needs};

/// The exit status of the job's command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    /// Stopped by the time limit.
    TimedOut,
    /// Ended without an exit code (killed by a signal or by the launcher).
    Killed,
}

/// Failure of a launcher step.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// Something the launcher requires is absent. It never repairs this itself.
    #[error("launcher `{launcher}` is not set up: {what}")]
    NotProvisioned { launcher: String, what: String },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// One live environment: provisioned, loaded, ready to run one job.
pub trait Environment {
    /// Run the job's command to completion (or until the time limit).
    fn run(&mut self, spec: &JobSpec) -> Result<Exit, LaunchError>;
    /// What the environment captured of the job's stdout and stderr (nothing, by default).
    fn streams(&mut self) -> crate::streams::Streams {
        crate::streams::Streams::default()
    }
    /// Copy the declared outputs into `results` on the host.
    fn collect(&mut self, spec: &JobSpec, results: &Path) -> Result<(), LaunchError>;
    /// Disband the environment. Implementations must also disband on `Drop` (panic, ctrl-c).
    fn destroy(self: Box<Self>) -> Result<(), LaunchError>;
}

/// A way of creating an [`Environment`].
pub trait Launcher {
    /// Stable id, as stored in config and used in the matrix `backend` column.
    fn id(&self) -> &str;
    /// Create the environment for `spec`.
    fn provision(&self, spec: &JobSpec) -> Result<Box<dyn Environment>, LaunchError>;
}

/// A launcher that was considered for some needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub id: String,
    /// Needs this launcher is not certified for; empty means it satisfies all of them.
    pub unmet: Vec<Capability>,
}

impl Provider {
    pub fn satisfies(&self) -> bool {
        self.unmet.is_empty()
    }
}

/// Why a launcher could not be opened.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    #[error("no launcher named `{0}` is registered")]
    Unknown(String),
    #[error("launcher `{id}` is not certified for: {}", list(.unmet))]
    Unmet { id: String, unmet: Vec<Capability> },
}

fn list(caps: &[Capability]) -> String {
    caps.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ")
}

/// "Now" and "here" for the engine: the clock and this machine's os and arch.
fn context() -> capant::Context {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    capant::Context::new(
        now,
        capant::Config::new()
            .with("os", std::env::consts::OS)
            .with("arch", std::env::consts::ARCH),
    )
}

fn capabilities<'a>(ids: impl Iterator<Item = &'a capant::NeedOutcome>) -> Vec<Capability> {
    ids.filter_map(|o| o.id.as_str().parse().ok()).collect()
}

/// The registered launchers and the matrix that certifies them.
pub struct Registry {
    launchers: Vec<Box<dyn Launcher>>,
    matrix: Matrix,
    table: Vec<capant::Provider>,
    ctx: capant::Context,
}

impl Registry {
    pub fn new(launchers: Vec<Box<dyn Launcher>>, matrix: Matrix) -> Self {
        let table = launchers
            .iter()
            .map(|l| {
                let surface = capant::Surface::new(l.id(), "1", capant::Config::new());
                capant::Provider::new(l.id(), vec![surface]).expect("one surface")
            })
            .collect();
        Registry {
            launchers,
            matrix,
            table,
            ctx: context(),
        }
    }

    fn evaluation(&self, needs: &Needs) -> capant::Evaluation {
        let need = capant::Need::new(needs.capabilities.iter().map(|c| c.cap_id()).collect())
            .prefer_providers(needs.prefer.clone());
        capant::evaluate(&self.table, &need, &self.matrix, &self.ctx)
            .expect("launcher ids are unique")
    }

    /// Every registered launcher, satisfying ones first. Within each group the spec's `prefer`
    /// order comes first, then registry order.
    pub fn providers(&self, needs: &Needs) -> Vec<Provider> {
        self.evaluation(needs)
            .entries()
            .iter()
            .map(|e| Provider {
                id: e.provider.clone(),
                unmet: capabilities(e.unmet()),
            })
            .collect()
    }

    /// Re-check the matrix and return the named launcher, or say what it lacks.
    pub fn open_named(&self, id: &str, needs: &Needs) -> Result<&dyn Launcher, OpenError> {
        match self.evaluation(needs).check(id) {
            Ok(_) => Ok(self
                .launchers
                .iter()
                .find(|l| l.id() == id)
                .expect("the table is built from the launchers")
                .as_ref()),
            Err(capant::CheckError::Rejected(u)) => Err(OpenError::Unmet {
                id: id.to_owned(),
                unmet: capabilities(u.missing.iter()),
            }),
            Err(_) => Err(OpenError::Unknown(id.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub(&'static str);
    impl Launcher for Stub {
        fn id(&self) -> &str {
            self.0
        }
        fn provision(&self, _: &JobSpec) -> Result<Box<dyn Environment>, LaunchError> {
            Err(LaunchError::Other("stub".into()))
        }
    }

    fn row(launcher: &str, cap: &str, yes: bool) -> capant::Row {
        let state = if yes {
            capant::State::Yes
        } else {
            capant::State::No(capant::Reason::Other("broken".into()))
        };
        capant::Row::capability(
            capant::CapabilityId::new(cap).unwrap(),
            launcher,
            "1",
            state,
        )
        .evidence(capant::Evidence::probe("t"))
    }

    fn registry() -> Registry {
        let mut t = Vec::new();
        for c in ["isolation.no-read-outside", "network.off"] {
            t.push(row("strong", c, true));
        }
        t.push(row("weak", "network.off", true));
        t.push(row("weak", "isolation.no-read-outside", false));
        t.push(row("other", "isolation.no-read-outside", true));
        t.push(row("other", "network.off", true));
        Registry::new(
            vec![
                Box::new(Stub("weak")),
                Box::new(Stub("other")),
                Box::new(Stub("strong")),
            ],
            Matrix::from_rows(t),
        )
    }

    fn needs(prefer: &[&str]) -> Needs {
        Needs {
            capabilities: vec![Capability::NoReadOutside, Capability::NetworkOff],
            prefer: prefer.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn unmet_needs_are_named_and_satisfying_launchers_come_first() {
        let p = Registry::providers(&registry(), &needs(&[]));
        let ids: Vec<_> = p.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["other", "strong", "weak"],
            "satisfying in registry order, then the rest"
        );
        assert_eq!(p[2].unmet, vec![Capability::NoReadOutside]);
    }

    #[test]
    fn prefer_orders_within_the_satisfying_group_only() {
        let p = registry().providers(&needs(&["weak", "strong"]));
        let ids: Vec<_> = p.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["strong", "other", "weak"],
            "weak is preferred but does not satisfy"
        );
    }

    #[test]
    fn a_yes_without_a_probe_grants_nothing() {
        // Control for the new rule: the old matrix trusted any `yes`; CapAnt wants the probe named.
        let bare = capant::Row::capability(
            Capability::NetworkOff.cap_id(),
            "strong",
            "1",
            capant::State::Yes,
        );
        let r = Registry::new(
            vec![Box::new(Stub("strong"))],
            Matrix::from_rows(vec![bare]),
        );
        let n = Needs {
            capabilities: vec![Capability::NetworkOff],
            prefer: vec![],
        };
        assert_eq!(r.providers(&n)[0].unmet, vec![Capability::NetworkOff]);
    }

    #[test]
    fn open_named_rechecks_and_refuses_with_the_reason() {
        let r = registry();
        assert!(r.open_named("strong", &needs(&[])).is_ok());
        assert_eq!(
            r.open_named("weak", &needs(&[])).err(),
            Some(OpenError::Unmet {
                id: "weak".into(),
                unmet: vec![Capability::NoReadOutside]
            })
        );
        assert_eq!(
            r.open_named("nope", &needs(&[])).err(),
            Some(OpenError::Unknown("nope".into()))
        );
    }
}
