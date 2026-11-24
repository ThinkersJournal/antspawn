// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-group egress lists (`docs/RUNNER-NETWORK.md`, "Per-project exceptions", ROADMAP row 43).
//!
//! A group's effective list is the default list plus the `egress = [...]` entries on the group. **An entry is
//! refused unless `egress-approved.toml` approves it by name, for every repo the group serves**: so an exception
//! is per project, approved, and visible in review. A wildcard entry also needs `wildcard = true` in its approval.
//! Nothing here opens a socket: the proxy (`crate::egress`) takes the resulting [`Policy`].

use serde::Deserialize;

use super::github::{AdapterConfig, RunnerGroup};
use crate::egress::{ALLOWED_PORT, Pattern, Policy, Probe};

/// Hosts every group may reach: GitHub's essential, action-download, log/artifact and cache hosts, and crates.io
/// (`docs/RUNNER-NETWORK.md`, "The hosts"). The runner self-update hosts are deliberately absent.
pub const DEFAULT_ALLOW: &[&str] = &[
    "github.com",
    "api.github.com",
    "codeload.github.com",
    "*.actions.githubusercontent.com",
    "*.blob.core.windows.net",
    "index.crates.io",
    "static.crates.io",
    "crates.io",
];

/// `host:443` or `*.domain:443` to the host part, or why not. Any other port, scheme, path or shape is refused.
pub fn entry_host(entry: &str) -> Result<String, String> {
    let e = entry.trim();
    let Some((host, port)) = e.rsplit_once(':') else {
        return Err(format!("`{entry}` must be host:{ALLOWED_PORT}"));
    };
    if port != ALLOWED_PORT.to_string() {
        return Err(format!("`{entry}`: only port {ALLOWED_PORT} is allowed"));
    }
    Pattern::parse(host).map_err(|e| format!("`{entry}`: {e}"))?;
    Ok(host.to_ascii_lowercase())
}

/// One approved exception: this host, for this repo, by this decision.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub host: String,
    pub repo: String,
    /// The decision: `board N / date` or CireSnave's words with the date.
    pub approved: String,
    pub reason: String,
    /// Must be true to approve a `*.domain` entry.
    #[serde(default)]
    pub wildcard: bool,
}

/// `egress-approved.toml`: `[[approved]]` rows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approvals {
    #[serde(default)]
    pub approved: Vec<Approval>,
}

impl Approvals {
    pub fn from_toml(text: &str) -> Result<Approvals, String> {
        let a: Approvals = toml::from_str(text).map_err(|e| e.to_string())?;
        for r in &a.approved {
            if [&r.host, &r.repo, &r.approved, &r.reason]
                .iter()
                .any(|s| s.trim().is_empty())
            {
                return Err(format!(
                    "approval for `{}` / `{}` has an empty field",
                    r.host, r.repo
                ));
            }
        }
        Ok(a)
    }

    fn covers(&self, host: &str, repo: &str, wildcard: bool) -> bool {
        self.approved.iter().any(|r| {
            r.host.eq_ignore_ascii_case(host) && r.repo == repo && (!wildcard || r.wildcard)
        })
    }
}

/// A group's resolved list.
#[derive(Debug, Clone)]
pub struct GroupEgress {
    pub group: String,
    /// The approved extras, as written in the config.
    pub extra: Vec<String>,
    pub policy: Policy,
}

fn group_egress(g: &RunnerGroup, approvals: &Approvals) -> Result<GroupEgress, String> {
    let mut list: Vec<String> = DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect();
    for e in &g.egress {
        let host = entry_host(e)?;
        let wildcard = host.starts_with("*.");
        for repo in &g.repos {
            if !approvals.covers(&host, repo, wildcard) {
                return Err(format!(
                    "group `{}`: egress `{e}` is not approved for repo `{repo}` in egress-approved.toml{}",
                    g.name,
                    if wildcard {
                        " (a wildcard needs `wildcard = true` in its approval)"
                    } else {
                        ""
                    }
                ));
            }
        }
        if !list.contains(&host) {
            list.push(host);
        }
    }
    Ok(GroupEgress {
        group: g.name.clone(),
        extra: g.egress.clone(),
        policy: Policy::parse_list(&list)?,
    })
}

/// Every group's effective policy, or the first unapproved entry.
pub fn effective(cfg: &AdapterConfig, approvals: &Approvals) -> Result<Vec<GroupEgress>, String> {
    cfg.groups
        .iter()
        .map(|g| group_egress(g, approvals))
        .collect()
}

/// What `spawn adapter --check` prints for one group.
pub fn describe(g: &GroupEgress) -> String {
    let extra = if g.extra.is_empty() {
        "none".to_string()
    } else {
        g.extra.join(", ")
    };
    let hosts: Vec<String> = g
        .policy
        .patterns()
        .iter()
        .map(|p| match p {
            Pattern::Exact(h) => format!("{h}:{ALLOWED_PORT}"),
            Pattern::Under(s) => format!("*.{s}:{ALLOWED_PORT}"),
        })
        .collect();
    format!(
        "group `{}`: {} default hosts, extra: {extra}; effective list: {}",
        g.group,
        DEFAULT_ALLOW.len(),
        hosts.join(", ")
    )
}

/// `--check` lines and problems for each group's listener. `probe` is injected so tests need no socket.
/// A group with no `egress_listen` has no listener to probe (a line says so); a listener that does not answer the
/// refusal to a name on no list is a problem: a runner behind it would have no way out, or one that does not filter.
pub fn listener_report(
    cfg: &AdapterConfig,
    probe: &dyn Fn(std::net::SocketAddr) -> Probe,
) -> (Vec<String>, Vec<String>) {
    let (mut lines, mut problems) = (Vec::new(), Vec::new());
    for g in &cfg.groups {
        let Some(l) = &g.egress_listen else {
            lines.push(format!(
                "group `{}`: no egress_listen, nothing to probe",
                g.name
            ));
            continue;
        };
        let Ok(addr) = l.parse::<std::net::SocketAddr>() else {
            problems.push(format!(
                "group `{}`: egress_listen `{l}` is not ip:port",
                g.name
            ));
            continue;
        };
        match probe(addr) {
            Probe::Filtering => lines.push(format!(
                "group `{}`: egress listener {addr} answers 403 to a name on no list",
                g.name
            )),
            Probe::Down(why) => problems.push(format!(
                "group `{}`: egress listener {addr} is down ({why}); start spawn-egress for this group",
                g.name
            )),
            Probe::Wrong(why) => problems.push(format!(
                "group `{}`: egress listener {addr} is not a filtering proxy ({why})",
                g.name
            )),
        }
    }
    (lines, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(egress: &str) -> AdapterConfig {
        AdapterConfig::from_toml(&format!(
            r#"
org = "ThinkersJournal"
app_id = 1
installation_id = 2
key_path = "C:/k.pem"
job_timeout_s = 3600

[[group]]
name = "g"
repos = ["ThinkersJournal/a", "ThinkersJournal/b"]
labels = ["x"]
launcher = "windows-sandbox"
{egress}
"#
        ))
        .unwrap()
    }

    fn approvals(hosts: &[(&str, &str, bool)]) -> Approvals {
        Approvals {
            approved: hosts
                .iter()
                .map(|(h, r, w)| Approval {
                    host: h.to_string(),
                    repo: format!("ThinkersJournal/{r}"),
                    approved: "board 1 / 2026-11-21".into(),
                    reason: "test".into(),
                    wildcard: *w,
                })
                .collect(),
        }
    }

    #[test]
    fn no_extras_means_the_default_list_only() {
        let g = effective(&cfg(""), &Approvals::default()).unwrap();
        assert!(g[0].policy.allows("api.github.com") && g[0].policy.allows("index.crates.io"));
        assert!(!g[0].policy.allows("example.com"));
        // the runner self-update hosts are deliberately not on the default list
        assert!(!g[0].policy.allows("objects.githubusercontent.com"));
    }

    #[test]
    fn an_unapproved_entry_is_refused_and_the_same_entry_passes_once_approved() {
        let c = cfg("egress = [\"files.example.org:443\"]");
        let e = effective(&c, &Approvals::default()).unwrap_err();
        assert!(
            e.contains("not approved") && e.contains("files.example.org:443"),
            "{e}"
        );
        // approved for one repo of two is still refused: the exception is per project
        let one = approvals(&[("files.example.org", "a", false)]);
        assert!(
            effective(&c, &one)
                .unwrap_err()
                .contains("ThinkersJournal/b")
        );
        let both = approvals(&[
            ("files.example.org", "a", false),
            ("files.example.org", "b", false),
        ]);
        let g = effective(&c, &both).unwrap();
        assert!(g[0].policy.allows("files.example.org"));
    }

    #[test]
    fn a_wildcard_needs_an_approval_that_says_so() {
        let c = cfg("egress = [\"*.example.org:443\"]");
        let plain = approvals(&[("*.example.org", "a", false), ("*.example.org", "b", false)]);
        assert!(effective(&c, &plain).unwrap_err().contains("wildcard"));
        let named = approvals(&[("*.example.org", "a", true), ("*.example.org", "b", true)]);
        assert!(
            effective(&c, &named).unwrap()[0]
                .policy
                .allows("x.example.org")
        );
    }

    #[test]
    fn entries_must_be_host_443() {
        for bad in [
            "example.org",
            "example.org:80",
            "https://example.org:443",
            "1.2.3.4:443",
            "*:443",
            "*.org:443",
            "example.org:443/x",
        ] {
            assert!(entry_host(bad).is_err(), "{bad}");
        }
        assert_eq!(
            entry_host("Files.Example.org:443").unwrap(),
            "files.example.org"
        );
        // the config validator applies the same rule at load time
        let bad = AdapterConfig::from_toml(
            "org = \"O\"\napp_id = 1\ninstallation_id = 2\nkey_path = \"k\"\njob_timeout_s = 60\n[[group]]\nname = \"g\"\nrepos = [\"O/a\"]\nlabels = [\"x\"]\nlauncher = \"windows-sandbox\"\negress = [\"example.org:80\"]\n",
        );
        assert!(bad.unwrap_err().to_string().contains("only port 443"));
    }

    #[test]
    fn the_approvals_file_is_strict() {
        assert!(Approvals::from_toml("[[approved]]\nhost = \"a.example.org\"\nrepo = \"O/a\"\napproved = \"board 9\"\nreason = \"r\"\n").is_ok());
        assert!(Approvals::from_toml("[[approved]]\nhost = \"a.example.org\"\nrepo = \"O/a\"\napproved = \"\"\nreason = \"r\"\n").is_err());
        assert!(Approvals::from_toml("[[approved]]\nhost = \"a.example.org\"\nrepo = \"O/a\"\napproved = \"b\"\nreason = \"r\"\nextra = 1\n").is_err());
    }

    #[test]
    fn describe_names_the_extras() {
        let c = cfg("egress = [\"files.example.org:443\"]");
        let both = approvals(&[
            ("files.example.org", "a", false),
            ("files.example.org", "b", false),
        ]);
        let d = describe(&effective(&c, &both).unwrap()[0]);
        assert!(
            d.contains("files.example.org:443") && d.contains("group `g`"),
            "{d}"
        );
    }

    #[test]
    fn describe_lists_every_effective_host() {
        let c = cfg("");
        let d = describe(&effective(&c, &Approvals::default()).unwrap()[0]);
        for h in DEFAULT_ALLOW {
            assert!(d.contains(h), "{h} missing from {d}");
        }
    }

    fn listen_cfg(listen: &str) -> AdapterConfig {
        cfg(&format!("egress_listen = \"{listen}\""))
    }

    #[test]
    fn listener_report_flags_a_listener_that_is_down_or_does_not_filter() {
        let c = listen_cfg("127.0.0.1:3128");
        let (l, p) = listener_report(&c, &|_| Probe::Filtering);
        assert!(p.is_empty() && l[0].contains("answers 403"), "{l:?} {p:?}");
        let (_, p) = listener_report(&c, &|_| Probe::Down("refused".into()));
        assert!(p.len() == 1 && p[0].contains("is down"), "{p:?}");
        let (_, p) = listener_report(&c, &|_| Probe::Wrong("answered `HTTP/1.1 200`".into()));
        assert!(
            p.len() == 1 && p[0].contains("not a filtering proxy"),
            "{p:?}"
        );
        // a group with no listener is a line, not a problem
        let (l, p) = listener_report(&cfg(""), &|_| unreachable!());
        assert!(p.is_empty() && l[0].contains("nothing to probe"), "{l:?}");
    }

    #[test]
    fn egress_listen_must_be_private_and_unique() {
        let ok = AdapterConfig::from_toml(
            "org = \"O\"
app_id = 1
installation_id = 2
key_path = \"k\"
job_timeout_s = 60
[[group]]
name = \"g\"
repos = [\"O/a\"]
labels = [\"x\"]
launcher = \"windows-sandbox\"
egress_listen = \"10.77.0.1:3128\"
",
        );
        assert!(ok.is_ok(), "{ok:?}");
        for bad in [
            "0.0.0.0:3128",
            "8.8.8.8:3128",
            "10.77.0.1:0",
            "10.77.0.1",
            "[::1]:3128",
        ] {
            let r = AdapterConfig::from_toml(&format!(
                "org = \"O\"
app_id = 1
installation_id = 2
key_path = \"k\"
job_timeout_s = 60
[[group]]
name = \"g\"
repos = [\"O/a\"]
labels = [\"x\"]
launcher = \"windows-sandbox\"
egress_listen = \"{bad}\"
"
            ));
            assert!(r.is_err(), "{bad} was accepted");
        }
        let two = "org = \"O\"
app_id = 1
installation_id = 2
key_path = \"k\"
job_timeout_s = 60
[[group]]
name = \"g\"
repos = [\"O/a\"]
labels = [\"x\"]
launcher = \"windows-sandbox\"
egress_listen = \"10.77.0.1:3128\"
[[group]]
name = \"h\"
repos = [\"O/a\"]
labels = [\"y\"]
launcher = \"windows-sandbox\"
egress_listen = \"10.77.0.1:3128\"
";
        assert!(
            AdapterConfig::from_toml(two)
                .unwrap_err()
                .to_string()
                .contains("one listener per group")
        );
    }
}
