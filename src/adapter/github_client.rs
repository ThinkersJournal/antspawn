// SPDX-License-Identifier: MIT OR Apache-2.0
//! The real GitHub client behind [`GithubApi`]: signs an App JWT with the key from a
//! [`KeyProvider`], exchanges it for a one-hour installation token and calls the runner endpoints.
//! Design: `docs/ADAPTER-GITHUB.md`.
//!
//! Secrets never reach a log, an error message or a file: the key is a [`Secret`], the JWT and the
//! installation token are held as `Secret` too, and error text carries GitHub's `message` field only.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};

#[cfg(windows)]
use super::github::check_key_path;
use super::github::{GithubApi, GroupInfo, JitRequest, KeyError, KeyProvider, MintError, Secret};

pub const GITHUB_API: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = concat!("antspawn/", env!("CARGO_PKG_VERSION"));
/// A GitHub installation token lasts an hour; refresh well before that.
const TOKEN_LIFETIME_S: u64 = 3000;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Decode a PEM private key (PKCS#1 `RSA PRIVATE KEY` or PKCS#8 `PRIVATE KEY`) into a key pair.
fn rsa_key_pair(pem: &Secret) -> Result<RsaKeyPair, KeyError> {
    let text = pem.expose();
    let (label, pkcs8) = if text.contains("BEGIN RSA PRIVATE KEY") {
        ("RSA PRIVATE KEY", false)
    } else if text.contains("BEGIN PRIVATE KEY") {
        ("PRIVATE KEY", true)
    } else {
        return Err(KeyError::Malformed);
    };
    let body: String = text
        .lines()
        .map(str::trim)
        .skip_while(|l| *l != format!("-----BEGIN {label}-----"))
        .skip(1)
        .take_while(|l| *l != format!("-----END {label}-----"))
        .collect();
    let der = STANDARD.decode(body).map_err(|_| KeyError::Malformed)?;
    let pair = if pkcs8 {
        RsaKeyPair::from_pkcs8(&der)
    } else {
        RsaKeyPair::from_der(&der)
    };
    pair.map_err(|_| KeyError::Malformed)
}

/// The App JWT GitHub wants: RS256, `iss` = the App id, valid from a minute ago (clock skew) for
/// nine minutes (the maximum is ten).
pub fn app_jwt(app_id: u64, pem: &Secret, now_s: u64) -> Result<Secret, KeyError> {
    let pair = rsa_key_pair(pem)?;
    let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = format!(
        r#"{{"iat":{},"exp":{},"iss":"{app_id}"}}"#,
        now_s.saturating_sub(60),
        now_s + 540
    );
    let signing_input = format!("{head}.{}", URL_SAFE_NO_PAD.encode(claims));
    let mut sig = vec![0u8; pair.public().modulus_len()];
    pair.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signing_input.as_bytes(),
        &mut sig,
    )
    .map_err(|_| KeyError::Malformed)?;
    Ok(Secret::new(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(sig)
    )))
}

/// One HTTP answer, reduced to what error mapping needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub retry_after_s: Option<u64>,
    /// `x-ratelimit-remaining` is `0`.
    pub quota_exhausted: bool,
    /// Seconds until `x-ratelimit-reset`, when given.
    pub reset_in_s: Option<u64>,
    pub body: String,
}

fn message(body: &str) -> String {
    let m = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str().map(str::to_owned)))
        .unwrap_or_else(|| "no message".into());
    m.chars().take(200).collect()
}

/// Map a non-2xx answer onto the error the minter's backoff understands.
pub fn classify(r: &Reply) -> MintError {
    let limited = r.status == 429
        || (r.status == 403 && (r.retry_after_s.is_some() || r.quota_exhausted))
        || message(&r.body).to_lowercase().contains("rate limit");
    if limited {
        let wait = r.retry_after_s.or(r.reset_in_s).unwrap_or(60).max(1);
        return MintError::RateLimited {
            retry_after_s: wait,
        };
    }
    match r.status {
        401 => MintError::Auth,
        500..=599 => MintError::Unavailable(format!("HTTP {}", r.status)),
        s => MintError::Rejected(format!("HTTP {s}: {}", message(&r.body))),
    }
}

/// A registered runner as the org lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerInfo {
    pub id: u64,
    pub name: String,
    pub status: String,
    pub busy: bool,
}

/// The HTTP client. Holds the installation token in memory only.
pub struct HttpGithub<K: KeyProvider> {
    base: String,
    app_id: u64,
    installation_id: u64,
    keys: K,
    agent: ureq::Agent,
    now: fn() -> u64,
    token: Mutex<Option<(Secret, u64)>>,
    groups: Mutex<HashMap<String, u64>>,
}

impl<K: KeyProvider> HttpGithub<K> {
    pub fn new(app_id: u64, installation_id: u64, keys: K) -> Self {
        Self::with_base(GITHUB_API, app_id, installation_id, keys, unix_now)
    }

    /// `base` is overridable so tests can point at a local fake.
    pub fn with_base(
        base: &str,
        app_id: u64,
        installation_id: u64,
        keys: K,
        now: fn() -> u64,
    ) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        HttpGithub {
            base: base.trim_end_matches('/').to_owned(),
            app_id,
            installation_id,
            keys,
            agent,
            now,
            token: Mutex::new(None),
            groups: Mutex::new(HashMap::new()),
        }
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        bearer: &Secret,
        body: Option<String>,
    ) -> Result<Reply, MintError> {
        let url = format!("{}{path}", self.base);
        let auth = format!("Bearer {}", bearer.expose());
        macro_rules! common {
            ($req:expr) => {
                $req.header("Authorization", &auth)
                    .header("Accept", "application/vnd.github+json")
                    .header("User-Agent", USER_AGENT)
                    .header("X-GitHub-Api-Version", API_VERSION)
            };
        }
        let sent = match (method, body) {
            ("GET", _) => common!(self.agent.get(&url)).call(),
            ("DELETE", _) => common!(self.agent.delete(&url)).call(),
            ("POST", b) => common!(self.agent.post(&url))
                .header("Content-Type", "application/json")
                .send(b.unwrap_or_default()),
            _ => return Err(MintError::Rejected(format!("unsupported method {method}"))),
        };
        let mut resp = sent.map_err(|e| MintError::Unavailable(transport(&e)))?;
        let h = |n: &str| {
            resp.headers()
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let now = (self.now)();
        let reply = Reply {
            status: resp.status().as_u16(),
            retry_after_s: h("retry-after").and_then(|v| v.parse().ok()),
            quota_exhausted: h("x-ratelimit-remaining").is_some_and(|v| v == "0"),
            reset_in_s: h("x-ratelimit-reset")
                .and_then(|v| v.parse::<u64>().ok())
                .map(|t| t.saturating_sub(now)),
            body: String::new(),
        };
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| MintError::Unavailable(transport(&e)))?;
        Ok(Reply { body, ..reply })
    }

    fn json(&self, r: Reply) -> Result<serde_json::Value, MintError> {
        if !(200..300).contains(&r.status) {
            return Err(classify(&r));
        }
        serde_json::from_str(&r.body)
            .map_err(|_| MintError::Unavailable("GitHub sent a non-JSON body".into()))
    }

    /// The installation token, exchanged from a fresh App JWT when absent or near expiry.
    fn installation_token(&self) -> Result<Secret, MintError> {
        let now = (self.now)();
        let mut cached = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, expires)) = cached.as_ref()
            && *expires > now + 300
        {
            return Ok(Secret::new(t.expose().to_owned()));
        }
        let pem = self.keys.load()?;
        let jwt = app_jwt(self.app_id, &pem, now)?;
        drop(pem);
        let reply = self.send(
            "POST",
            &format!("/app/installations/{}/access_tokens", self.installation_id),
            &jwt,
            Some("{}".into()),
        )?;
        if reply.status == 401 || reply.status == 403 && !reply.quota_exhausted {
            return Err(MintError::Auth);
        }
        let v = self.json(reply)?;
        let token = v
            .get("token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| MintError::Unavailable("no token in the answer".into()))?;
        *cached = Some((Secret::new(token.to_owned()), now + TOKEN_LIFETIME_S));
        Ok(Secret::new(token.to_owned()))
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<serde_json::Value, MintError> {
        let token = self.installation_token()?;
        let reply = self.send(method, path, &token, body)?;
        if reply.status == 204 {
            return Ok(serde_json::Value::Null);
        }
        self.json(reply)
    }

    fn group_id(&self, org: &str, group: &str) -> Result<(u64, String), MintError> {
        let v = self.call(
            "GET",
            &format!("/orgs/{org}/actions/runner-groups?per_page=100"),
            None,
        )?;
        let found = v["runner_groups"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|g| g["name"] == group)
            .ok_or_else(|| MintError::Rejected(format!("runner group `{group}` does not exist")))?;
        let id = found["id"]
            .as_u64()
            .ok_or_else(|| MintError::Unavailable("a group without an id".into()))?;
        let visibility = found["visibility"].as_str().unwrap_or("").to_owned();
        self.groups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(group.to_owned(), id);
        Ok((id, visibility))
    }

    /// Like [`GithubApi::generate_jitconfig`], but also returns the runner's id (needed to remove it).
    pub fn generate_jitconfig_with_id(&self, req: &JitRequest) -> Result<(u64, Secret), MintError> {
        let cached = self
            .groups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&req.group)
            .copied();
        let gid = match cached {
            Some(id) => id,
            None => self.group_id(&req.org, &req.group)?.0,
        };
        let body = serde_json::json!({
            "name": req.runner_name,
            "runner_group_id": gid,
            "labels": req.labels,
            "work_folder": "_work",
        });
        let v = self.call(
            "POST",
            &format!("/orgs/{}/actions/runners/generate-jitconfig", req.org),
            Some(body.to_string()),
        )?;
        let id = v["runner"]["id"]
            .as_u64()
            .ok_or_else(|| MintError::Unavailable("no runner id in the answer".into()))?;
        let cfg = v["encoded_jit_config"]
            .as_str()
            .ok_or_else(|| MintError::Unavailable("no jit config in the answer".into()))?;
        Ok((id, Secret::new(cfg.to_owned())))
    }

    /// The org's registered runners (first 10 pages of 100).
    pub fn list_runners(&self, org: &str) -> Result<Vec<RunnerInfo>, MintError> {
        let mut out = Vec::new();
        for page in 1..=10 {
            let v = self.call(
                "GET",
                &format!("/orgs/{org}/actions/runners?per_page=100&page={page}"),
                None,
            )?;
            let batch = v["runners"].as_array().cloned().unwrap_or_default();
            let n = batch.len();
            out.extend(batch.iter().filter_map(|r| {
                Some(RunnerInfo {
                    id: r["id"].as_u64()?,
                    name: r["name"].as_str()?.to_owned(),
                    status: r["status"].as_str().unwrap_or("").to_owned(),
                    busy: r["busy"].as_bool().unwrap_or(false),
                })
            }));
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }

    /// Remove one runner (`DELETE /orgs/{org}/actions/runners/{id}`, 204).
    pub fn delete_runner(&self, org: &str, runner_id: u64) -> Result<(), MintError> {
        self.call(
            "DELETE",
            &format!("/orgs/{org}/actions/runners/{runner_id}"),
            None,
        )
        .map(|_| ())
    }
}

/// Pages of 100 read per listing; anything beyond waits for a later tick.
const MAX_PAGES: u32 = 5;

/// The ids of the runs in one page of `/actions/runs`.
fn run_ids(v: &serde_json::Value) -> Vec<u64> {
    v["workflow_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["id"].as_u64())
        .collect()
}

/// The queued jobs in one page of `/actions/runs/{id}/jobs`. A job missing its id or labels is skipped:
/// a job we cannot route is never one we should mint for.
fn queued_jobs(repo: &str, run_id: u64, v: &serde_json::Value) -> Vec<super::watcher::QueuedJob> {
    v["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|j| j["status"] == "queued")
        .filter_map(|j| {
            Some(super::watcher::QueuedJob {
                repo: repo.to_owned(),
                run_id,
                job_id: j["id"].as_u64()?,
                labels: j["labels"]
                    .as_array()?
                    .iter()
                    .filter_map(|l| l.as_str().map(str::to_owned))
                    .collect(),
            })
        })
        .collect()
}

impl<K: KeyProvider> HttpGithub<K> {
    /// Run ids with the given status, as GitHub lists them.
    fn runs_with_status(&self, repo: &str, status: &str) -> Result<Vec<u64>, MintError> {
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let v = self.call(
                "GET",
                &format!("/repos/{repo}/actions/runs?status={status}&per_page=100&page={page}"),
                None,
            )?;
            let ids = run_ids(&v);
            let n = ids.len();
            out.extend(ids);
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }
}

impl<K: KeyProvider> super::watcher::JobSource for HttpGithub<K> {
    /// Queued jobs of `repo`. A run that is itself `queued` or `in_progress` can hold queued jobs (a
    /// later job of a multi-job workflow waits while an earlier one runs), so both are read. A run
    /// that vanishes between the two calls (404) is skipped; any other failure fails the whole poll,
    /// so the watcher does not forget jobs it merely could not see.
    fn queued(&self, repo: &str) -> Result<Vec<super::watcher::QueuedJob>, MintError> {
        let mut run_ids = self.runs_with_status(repo, "queued")?;
        for id in self.runs_with_status(repo, "in_progress")? {
            if !run_ids.contains(&id) {
                run_ids.push(id);
            }
        }
        let mut jobs = Vec::new();
        for run in run_ids {
            for page in 1..=MAX_PAGES {
                let token = self.installation_token()?;
                let path = format!(
                    "/repos/{repo}/actions/runs/{run}/jobs?filter=latest&per_page=100&page={page}"
                );
                let reply = self.send("GET", &path, &token, None)?;
                if reply.status == 404 {
                    break;
                }
                let v = self.json(reply)?;
                jobs.extend(queued_jobs(repo, run, &v));
                if v["jobs"].as_array().map_or(0, Vec::len) < 100 {
                    break;
                }
            }
        }
        Ok(jobs)
    }
}

/// A transport error without the URL (which is harmless but noisy) or any header.
fn transport(e: &ureq::Error) -> String {
    format!("transport: {e}")
}

impl<K: KeyProvider> GithubApi for HttpGithub<K> {
    fn runner_group(&self, org: &str, group: &str) -> Result<GroupInfo, MintError> {
        let (id, visibility) = self.group_id(org, group)?;
        let repos = if visibility == "selected" {
            let v = self.call(
                "GET",
                &format!("/orgs/{org}/actions/runner-groups/{id}/repositories?per_page=100"),
                None,
            )?;
            v["repositories"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["full_name"].as_str().map(str::to_owned))
                .collect()
        } else {
            Vec::new()
        };
        Ok(GroupInfo { visibility, repos })
    }

    fn generate_jitconfig(&self, req: &JitRequest) -> Result<Secret, MintError> {
        self.generate_jitconfig_with_id(req).map(|(_, s)| s)
    }
}

/// Reads the App key from a DPAPI-protected file as `ConvertFrom-SecureString` writes it (hex of
/// the blob; the plaintext is the PEM as UTF-16LE). Only the account that wrote the file can decrypt
/// it. The path is checked on construction and on every load, like [`super::github::FileKeyProvider`].
#[cfg(windows)]
pub struct DpapiKeyProvider {
    path: std::path::PathBuf,
    sandbox_roots: Vec<std::path::PathBuf>,
}

#[cfg(windows)]
impl DpapiKeyProvider {
    pub fn new(
        path: std::path::PathBuf,
        sandbox_roots: Vec<std::path::PathBuf>,
    ) -> Result<Self, KeyError> {
        check_key_path(&path, &sandbox_roots)?;
        Ok(DpapiKeyProvider {
            path,
            sandbox_roots,
        })
    }
}

#[cfg(windows)]
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    #[cfg(test)]
    use windows_sys::Win32::Security::Cryptography::CryptProtectData;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};

    const UI_FORBIDDEN: u32 = 1;

    fn blob(b: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: b.len() as u32,
            pbData: b.as_ptr() as *mut u8,
        }
    }

    /// Take ownership of a DPAPI output blob: copy it, wipe it, free it.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: DPAPI returned `cbData` bytes at `pbData`, allocated with LocalAlloc.
        unsafe {
            let slice = std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize);
            let v = slice.to_vec();
            slice.fill(0);
            LocalFree(out.pbData as _);
            v
        }
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: `input` points into `data`, which outlives the call; `out` is written by the API.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                UI_FORBIDDEN,
                &mut out,
            )
        };
        // SAFETY: on success `out` is a LocalAlloc'd blob we now own.
        (ok != 0).then(|| unsafe { take(out) })
    }

    /// Test support: protect `data` for the current user, as PowerShell does.
    #[cfg(test)]
    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as in `unprotect`.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                UI_FORBIDDEN,
                &mut out,
            )
        };
        // SAFETY: as in `unprotect`.
        (ok != 0).then(|| unsafe { take(out) })
    }
}

#[cfg(windows)]
impl KeyProvider for DpapiKeyProvider {
    fn load(&self) -> Result<Secret, KeyError> {
        check_key_path(&self.path, &self.sandbox_roots)?;
        let unreadable = || KeyError::Unreadable(self.path.clone());
        let text = std::fs::read_to_string(&self.path).map_err(|_| unreadable())?;
        let blob = unhex(text.trim()).ok_or(KeyError::Malformed)?;
        let plain = dpapi::unprotect(&blob).ok_or_else(unreadable)?;
        let units: Vec<u16> = plain
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16(&units)
            .map(Secret::new)
            .map_err(|_| KeyError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn run_ids_reads_ids_and_skips_malformed() {
        let v = serde_json::json!({"workflow_runs": [{"id": 7}, {"id": "x"}, {}, {"id": 9}]});
        assert_eq!(run_ids(&v), vec![7, 9]);
        // Positive control: an unrelated shape yields nothing rather than panicking.
        assert!(run_ids(&serde_json::json!({"message": "Not Found"})).is_empty());
    }

    #[test]
    fn queued_jobs_keeps_only_queued_with_labels() {
        let v = serde_json::json!({"jobs": [
            {"id": 1, "status": "queued", "labels": ["self-hosted", "antspawn"]},
            {"id": 2, "status": "in_progress", "labels": ["antspawn"]},
            {"id": 3, "status": "completed", "labels": ["antspawn"]},
            {"status": "queued", "labels": ["antspawn"]},
            {"id": 5, "status": "queued"},
        ]});
        let got = queued_jobs("O/r", 40, &v);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].job_id, 1);
        assert_eq!(got[0].run_id, 40);
        assert_eq!(got[0].repo, "O/r");
        assert_eq!(got[0].labels, vec!["self-hosted", "antspawn"]);
    }

    struct NoKey;
    impl KeyProvider for NoKey {
        fn load(&self) -> Result<Secret, KeyError> {
            Err(KeyError::Unreadable("C:/none".into()))
        }
    }

    fn reply(status: u16) -> Reply {
        Reply {
            status,
            retry_after_s: None,
            quota_exhausted: false,
            reset_in_s: None,
            body: r#"{"message":"nope"}"#.into(),
        }
    }

    #[test]
    fn statuses_map_to_the_errors_the_backoff_understands() {
        assert_eq!(classify(&reply(401)), MintError::Auth);
        assert_eq!(
            classify(&reply(502)),
            MintError::Unavailable("HTTP 502".into())
        );
        assert_eq!(
            classify(&reply(422)),
            MintError::Rejected("HTTP 422: nope".into())
        );
        // A plain 403 is a permissions refusal, not a rate limit (control for the next three).
        assert!(matches!(classify(&reply(403)), MintError::Rejected(_)));
        let mut r = reply(403);
        r.retry_after_s = Some(30);
        assert_eq!(classify(&r), MintError::RateLimited { retry_after_s: 30 });
        let mut r = reply(403);
        (r.quota_exhausted, r.reset_in_s) = (true, Some(120));
        assert_eq!(classify(&r), MintError::RateLimited { retry_after_s: 120 });
        assert!(matches!(
            classify(&reply(429)),
            MintError::RateLimited { .. }
        ));
    }

    #[test]
    fn error_text_carries_the_message_only() {
        let mut r = reply(422);
        r.body = format!(
            r#"{{"message":"{}","token":"ghs_SECRET"}}"#,
            "x".repeat(500)
        );
        let e = classify(&r).to_string();
        assert!(!e.contains("ghs_SECRET"), "{e}");
        assert!(e.len() < 260, "{} chars", e.len());
    }

    #[test]
    fn a_key_that_is_not_an_rsa_pem_is_malformed_and_never_echoed() {
        for junk in [
            "not a key SECRETBYTES",
            "-----BEGIN RSA PRIVATE KEY-----\n!!!SECRETBYTES!!!\n-----END RSA PRIVATE KEY-----",
            "-----BEGIN RSA PRIVATE KEY-----\nQUJD\n-----END RSA PRIVATE KEY-----",
        ] {
            let e = app_jwt(1, &Secret::new(junk.into()), 1_000).unwrap_err();
            assert_eq!(e, KeyError::Malformed);
            assert!(!e.to_string().contains("SECRETBYTES"));
        }
    }

    /// One-shot local server: answers each connection with the next canned reply and records the
    /// request head.
    fn serve(replies: Vec<(u16, &'static str)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in replies {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
            seen
        });
        (url, h)
    }

    #[test]
    fn a_missing_key_stops_before_any_request_is_made() {
        let (url, h) = serve(vec![]);
        let api = HttpGithub::with_base(&url, 1, 2, NoKey, || 1_000);
        let e = api.runner_group("o", "g").unwrap_err();
        assert!(matches!(e, MintError::Key(KeyError::Unreadable(_))), "{e}");
        assert!(h.join().unwrap().is_empty(), "no request may be sent");
    }

    #[test]
    fn transport_failure_is_unavailable_not_a_panic() {
        // Port 1 is closed: the connection is refused.
        let api = HttpGithub::with_base("http://127.0.0.1:1", 1, 2, NoKey, || 1_000);
        let e = api
            .send("GET", "/x", &Secret::new("t".into()), None)
            .unwrap_err();
        assert!(matches!(e, MintError::Unavailable(_)), "{e}");
    }

    #[test]
    fn requests_carry_the_bearer_and_an_answer_is_parsed() {
        let (url, h) = serve(vec![(
            200,
            r#"{"runners":[{"id":7,"name":"r","status":"online","busy":false}]}"#,
        )]);
        let api = HttpGithub::with_base(&url, 1, 2, NoKey, || 1_000);
        // Seed the installation token cache so no key is needed.
        *api.token.lock().unwrap() = Some((Secret::new("tok123".into()), 1_000 + 3_000));
        let runners = api.list_runners("o").unwrap();
        assert_eq!(
            runners,
            vec![RunnerInfo {
                id: 7,
                name: "r".into(),
                status: "online".into(),
                busy: false
            }]
        );
        let head = h.join().unwrap().remove(0);
        assert!(
            head.starts_with("GET /orgs/o/actions/runners?per_page=100&page=1 "),
            "{head}"
        );
        let head = head.to_lowercase();
        assert!(head.contains("authorization: bearer tok123"), "{head}");
        assert!(head.contains("x-github-api-version: 2022-11-28"), "{head}");
        assert!(head.contains("user-agent: antspawn/"), "{head}");
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip_and_garbage_is_refused() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nQUJD\n-----END RSA PRIVATE KEY-----\n";
        let utf16: Vec<u8> = pem.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let blob = dpapi::protect(&utf16).expect("protect");
        let hex: String = blob.iter().map(|b| format!("{b:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("antspawn-dpapi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (good, bad, odd) = (
            dir.join("k.dpapi"),
            dir.join("bad.dpapi"),
            dir.join("odd.dpapi"),
        );
        std::fs::write(&good, &hex).unwrap();
        let mut broken = blob.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;
        std::fs::write(
            &bad,
            broken
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        )
        .unwrap();
        std::fs::write(&odd, "zz-not-hex").unwrap();

        let got = DpapiKeyProvider::new(good, vec![]).unwrap().load().unwrap();
        assert_eq!(got.expose(), pem);
        assert!(matches!(
            DpapiKeyProvider::new(bad, vec![]).unwrap().load(),
            Err(KeyError::Unreadable(_))
        ));
        assert!(matches!(
            DpapiKeyProvider::new(odd, vec![]).unwrap().load(),
            Err(KeyError::Malformed)
        ));
        // The sandbox-root refusal applies to this provider too.
        assert!(matches!(
            DpapiKeyProvider::new(dir.join("k.dpapi"), vec![dir.clone()]),
            Err(KeyError::InsideSandbox { .. })
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
