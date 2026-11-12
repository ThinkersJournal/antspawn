// SPDX-License-Identifier: MIT OR Apache-2.0
//! The account pool's lease allocator (slice A3 P2).
//!
//! A pool is an ordered list of restricted accounts ([`AccountIdentity`]). A session takes a free one
//! for its whole life; a second concurrent session takes another; when none is free the open is
//! refused naming `pool exhausted`.
//!
//! **The lease is a held file, not a record.** `<root>/leases/<account>.lease` is opened with share
//! mode 0 (no other opener, so a second launcher gets a sharing violation) and delete-on-close. The
//! operating system releases it when its owner goes, including a crash or a kill, so there is no
//! pid to go stale, no pid reuse, and no reclaim race between two openers. Dropping the [`Lease`]
//! releases it.
//!
//! The leases directory is created by the launcher user. Giving the pool accounts write access to
//! nothing under it is part of the per-account setup (P4); this module only needs the launcher's own
//! rights. The lease is Windows-only (share modes); elsewhere every acquire is refused.

use std::path::{Path, PathBuf};

use crate::launcher::{LaunchError, Launcher};
use crate::launchers::account::{Account, AccountIdentity};
use crate::runner::Layout;
use crate::session::{ExecOutcome, ExecRequest, Session};
use crate::session_proto::FileInfo;

/// Why a lease was not granted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PoolError {
    /// Every account in the pool is leased by a live owner.
    #[error("pool exhausted: all {0} account(s) are in use by live sessions")]
    Exhausted(usize),
    /// The pool itself is unusable (empty, a repeated or invalid name, not Windows).
    #[error("pool unusable: {0}")]
    Unusable(String),
    /// A lease could not be tried for a reason other than being held.
    #[error("lease of `{account}` failed: {why}")]
    Io { account: String, why: String },
}

impl From<PoolError> for LaunchError {
    fn from(e: PoolError) -> Self {
        LaunchError::Other(e.to_string())
    }
}

/// An ordered set of accounts and where their leases live.
#[derive(Debug, Clone)]
pub struct Pool {
    dir: PathBuf,
    members: Vec<AccountIdentity>,
}

/// A held lease on one account. Dropping it releases the account.
#[derive(Debug)]
pub struct Lease {
    identity: AccountIdentity,
    // Held open for its share mode; closing it deletes the file.
    _file: std::fs::File,
}

impl Lease {
    pub fn identity(&self) -> &AccountIdentity {
        &self.identity
    }
}

/// A lease file name is built from the account, so the account must be a plain name.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Pool {
    /// A pool of `members` (tried in order) with its leases under `layout.root/leases`.
    pub fn new(layout: &Layout, members: Vec<AccountIdentity>) -> Result<Pool, PoolError> {
        Pool::in_dir(layout.root.join("leases"), members)
    }

    pub fn in_dir(
        dir: impl Into<PathBuf>,
        members: Vec<AccountIdentity>,
    ) -> Result<Pool, PoolError> {
        if members.is_empty() {
            return Err(PoolError::Unusable("the pool has no accounts".into()));
        }
        for (i, m) in members.iter().enumerate() {
            if !plain(&m.account) {
                return Err(PoolError::Unusable(format!(
                    "`{}` is not a plain account name",
                    m.account
                )));
            }
            if members[..i].iter().any(|o| o.account == m.account) {
                return Err(PoolError::Unusable(format!(
                    "account `{}` is listed twice",
                    m.account
                )));
            }
            if members[..i].iter().any(|o| o.task == m.task) {
                return Err(PoolError::Unusable(format!(
                    "task `{}` is listed twice",
                    m.task
                )));
            }
        }
        Ok(Pool {
            dir: dir.into(),
            members,
        })
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Take the first free account.
    pub fn acquire(&self) -> Result<Lease, PoolError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| PoolError::Io {
            account: "-".into(),
            why: format!("cannot create {}: {e}", self.dir.display()),
        })?;
        for m in &self.members {
            if let Some(file) =
                try_hold(&self.dir.join(format!("{}.lease", m.account)), &m.account)?
            {
                return Ok(Lease {
                    identity: m.clone(),
                    _file: file,
                });
            }
        }
        Err(PoolError::Exhausted(self.members.len()))
    }
}

/// `Ok(None)`: someone else holds it. `Ok(Some)`: now ours.
#[cfg(windows)]
fn try_hold(path: &Path, account: &str) -> Result<Option<std::fs::File>, PoolError> {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_ACCESS_DENIED: i32 = 5;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
    {
        Ok(f) => Ok(Some(f)),
        // A holder that is mid-close (delete pending) answers access denied for a moment; both
        // that and a sharing violation mean "not free now", never "free".
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION | ERROR_ACCESS_DENIED)
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(PoolError::Io {
            account: account.into(),
            why: e.to_string(),
        }),
    }
}

#[cfg(not(windows))]
fn try_hold(_: &Path, _: &str) -> Result<Option<std::fs::File>, PoolError> {
    Err(PoolError::Unusable(
        "the account pool is Windows-only (leases are held with share modes)".into(),
    ))
}

/// A pool of restricted accounts as a session source: each [`AccountPool::open_session_in`] leases
/// an account, opens the account launcher's session on it, and releases the account when the session
/// closes or drops.
#[derive(Debug, Clone)]
pub struct AccountPool {
    layout: Layout,
    pool: Pool,
}

impl AccountPool {
    /// `layout` is the pool root (`%ProgramData%\antspawn-pool`): leases live under it and each
    /// account has its own tree at `<root>/<account>` (see [`AccountPool::account_layout`]), which
    /// `docs/proof/PASTE-pool-accounts.ps1` makes and whose task serves it with `--root`.
    pub fn new(layout: Layout, members: Vec<AccountIdentity>) -> Result<Self, PoolError> {
        let pool = Pool::new(&layout, members)?;
        Ok(AccountPool { layout, pool })
    }

    /// The spool/results/work tree of one pool account: no directory is shared between accounts, so
    /// one cannot read another's jobs and one runner never hosts a session meant for another.
    pub fn account_layout(&self, account: &str) -> Layout {
        Layout::new(self.layout.root.join(account))
    }

    /// Lease an account and open a session on it (`root` as for [`Launcher::open_session_in`]).
    ///
    /// The account is chosen here, so a workspace `bind` cannot know whom to grant before this call:
    /// [`LeasedSession::principal`] says who was leased, and wiring the grant to it is a later slice.
    pub fn open_session_in(&self, root: Option<&Path>) -> Result<LeasedSession, LaunchError> {
        let lease = self.pool.acquire()?;
        let launcher = Account::with_identity(
            self.account_layout(&lease.identity().account),
            lease.identity().clone(),
        );
        let inner = launcher.open_session_in(root)?;
        Ok(LeasedSession {
            inner: Some(inner),
            lease,
        })
    }
}

/// A session on a leased account. The account is released after the session is gone.
pub struct LeasedSession {
    inner: Option<Box<dyn Session>>,
    lease: Lease,
}

impl LeasedSession {
    /// The account this session runs as.
    pub fn principal(&self) -> &str {
        &self.lease.identity.account
    }

    fn session(&mut self) -> Result<&mut Box<dyn Session>, LaunchError> {
        self.inner
            .as_mut()
            .ok_or_else(|| LaunchError::Other("the session is closed".into()))
    }
}

impl Session for LeasedSession {
    fn exec(&mut self, req: &ExecRequest) -> Result<ExecOutcome, LaunchError> {
        self.session()?.exec(req)
    }

    fn close(mut self: Box<Self>) -> Result<(), LaunchError> {
        // The account is released only after the session ended (the lease drops with `self`).
        match self.inner.take() {
            Some(s) => s.close(),
            None => Ok(()),
        }
    }

    fn put(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<u64, LaunchError> {
        self.session()?.put(path, offset, data)
    }

    fn get(&mut self, path: &str, offset: u64, len: u64) -> Result<(Vec<u8>, bool), LaunchError> {
        self.session()?.get(path, offset, len)
    }

    fn list(&mut self, after: Option<&str>) -> Result<(Vec<FileInfo>, bool), LaunchError> {
        self.session()?.list(after)
    }
}

impl Drop for LeasedSession {
    fn drop(&mut self) {
        // End the session first; only then does the lease field drop and free the account.
        self.inner = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<AccountIdentity> {
        (1..=n)
            .map(|i| AccountIdentity::new(format!("antspawn-run{i}"), format!("AntSpawnRunner{i}")))
            .collect()
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antspawn-pool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn an_unusable_pool_is_refused_up_front() {
        let d = dir("bad");
        assert!(Pool::in_dir(&d, vec![]).is_err());
        let mut twice = ids(2);
        twice[1].account = twice[0].account.clone();
        assert!(Pool::in_dir(&d, twice).is_err());
        let mut same_task = ids(2);
        same_task[1].task = same_task[0].task.clone();
        assert!(Pool::in_dir(&d, same_task).is_err());
        let mut odd = ids(1);
        odd[0].account = "..\\x".into();
        assert!(Pool::in_dir(&d, odd).is_err());
        assert!(Pool::in_dir(&d, ids(2)).is_ok());
    }

    #[test]
    fn each_account_has_its_own_tree() {
        let p = AccountPool::new(Layout::new("R"), ids(2)).unwrap();
        let (a, b) = (
            p.account_layout("antspawn-run1"),
            p.account_layout("antspawn-run2"),
        );
        assert_eq!(a.root, PathBuf::from("R").join("antspawn-run1"));
        assert_ne!(a.spool(), b.spool());
        assert!(!a.work().starts_with(&b.root) && !b.work().starts_with(&a.root));
    }

    #[cfg(windows)]
    #[test]
    fn leases_are_distinct_the_third_is_refused_and_a_released_one_is_reused() {
        let d = dir("lease");
        let pool = Pool::in_dir(&d, ids(2)).unwrap();
        let a = pool.acquire().unwrap();
        let b = pool.acquire().unwrap();
        assert_ne!(a.identity().account, b.identity().account);
        let e = pool.acquire().unwrap_err();
        assert!(e.to_string().contains("pool exhausted"), "{e}");
        let first = a.identity().account.clone();
        drop(a);
        // Positive control: the same call that was refused now succeeds, on the freed account.
        let c = pool.acquire().unwrap();
        assert_eq!(c.identity().account, first);
        drop((b, c));
        assert!(
            std::fs::read_dir(&d).unwrap().next().is_none(),
            "released leases leave no file behind"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The owner being gone is the operating system's doing: kill a process that holds a lease
    /// and the account is free again, with nothing reclaimed by us.
    #[cfg(windows)]
    #[test]
    fn a_killed_owner_releases_its_lease() {
        let d = dir("kill");
        let pool = Pool::in_dir(&d, ids(1)).unwrap();
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join("antspawn-run1.lease");
        let script = format!(
            "$f=[IO.File]::Open('{}','OpenOrCreate','ReadWrite','None'); Start-Sleep 120",
            file.display()
        );
        let mut child = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .spawn()
            .unwrap();
        // Wait until the child really holds it (our own acquire is refused), then kill it.
        let start = std::time::Instant::now();
        loop {
            if matches!(pool.acquire(), Err(PoolError::Exhausted(1))) {
                break;
            }
            assert!(
                start.elapsed().as_secs() < 30,
                "the child never took the lease"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        let start = std::time::Instant::now();
        let got = loop {
            match pool.acquire() {
                Ok(l) => break l,
                Err(PoolError::Exhausted(_)) if start.elapsed().as_secs() < 10 => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => panic!("still held after the owner was killed: {e}"),
            }
        };
        drop(got);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_every_acquire_is_refused_loudly() {
        let pool = Pool::in_dir(dir("nowin"), ids(1)).unwrap();
        assert!(matches!(pool.acquire(), Err(PoolError::Unusable(_))));
    }
}
