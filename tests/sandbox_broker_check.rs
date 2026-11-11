// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-sandbox-broker --check`, the real binary, on real files: a good config is accepted and
//! described, a bad one is refused with exit 1 naming the field, a missing file and bad usage are
//! refused. Starts no sandbox and no server. NOT shown: serving.

use std::process::Command;

const GOOD: &str = r#"
pipe = "antspawn-sandbox-broker"
allow_sids = ["S-1-5-21-1-2-3-1001"]

[[profiles]]
name = "default"
template = 'C:\antspawn\sandbox\default.wsb'
"#;

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_spawn-sandbox-broker"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn file(tag: &str, text: &str) -> String {
    let p = std::env::temp_dir().join(format!("antspawn-sbc-{tag}-{}.toml", std::process::id()));
    std::fs::write(&p, text).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn a_good_config_is_accepted_and_described() {
    let f = file("good", GOOD);
    let (code, out, err) = run(&["--config", &f, "--check"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("config ok") && out.contains("default"),
        "{out}"
    );
    let _ = std::fs::remove_file(f);
}

#[test]
fn a_bad_config_is_refused_naming_the_field() {
    let f = file("bad", &GOOD.replace(".wsb", ".exe"));
    let (code, out, err) = run(&["--config", &f, "--check"]);
    assert_eq!(code, 1);
    assert!(err.contains("template") && out.is_empty(), "{err} / {out}");
    let _ = std::fs::remove_file(f);
}

#[test]
fn a_missing_file_and_bad_usage_are_refused() {
    let (code, _, err) = run(&["--config", "no-such-file.toml", "--check"]);
    assert_eq!(code, 1);
    assert!(err.contains("cannot read"), "{err}");
    assert_eq!(run(&[]).0, 2);
    assert_eq!(run(&["--check"]).0, 2);
    assert_eq!(run(&["--bogus"]).0, 2);
}

const EXAMPLE_CONFIG: &str = include_str!("../examples/sandbox/sandbox-broker.example.toml");
const EXAMPLE_TEMPLATES: [(&str, &str); 2] = [
    ("default", include_str!("../examples/sandbox/default.wsb")),
    ("staging", include_str!("../examples/sandbox/staging.wsb")),
];

/// The shipped example config is accepted by the real binary and offers exactly the shipped templates.
#[test]
fn the_shipped_example_config_is_accepted_and_offers_its_templates() {
    let f = file("example", EXAMPLE_CONFIG);
    let (code, out, err) = run(&["--config", &f, "--check"]);
    assert_eq!(code, 0, "{err}");
    for (name, _) in EXAMPLE_TEMPLATES {
        assert!(out.contains(name), "{out}");
    }
    assert_eq!(EXAMPLE_CONFIG.matches("[[profiles]]").count(), 2);
    let _ = std::fs::remove_file(f);
}

/// A fake `wsb` that records the configuration `start -c` was given and answers with an id.
struct Fake(std::cell::RefCell<Vec<String>>);

impl antspawn::wsb_driver::Wsb for &Fake {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        if args[0] == "start" {
            self.0.borrow_mut().push(args[2].to_string());
        }
        Ok("Id: 0f3a9c2e-1b4d-4e5f-8a6b-7c8d9e0f1a2b\n".to_string())
    }
}

/// The real driver starts each shipped file as it is on disk and hands `wsb` exactly its text (a file
/// that opened with a comment would be refused: the driver wants `<Configuration` first). Control: the
/// same file with a comment put in front is refused and never reaches `wsb`.
#[test]
fn the_driver_accepts_each_shipped_template_verbatim() {
    use antspawn::sandbox_broker::SandboxDriver;
    use antspawn::wsb_driver::WsbDriver;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/sandbox");
    let fake = Fake(Default::default());
    let mut templates = std::collections::BTreeMap::new();
    for (name, _) in EXAMPLE_TEMPLATES {
        templates.insert(name.to_string(), dir.join(format!("{name}.wsb")));
    }
    let bad = std::env::temp_dir().join(format!("antspawn-sbc-bad-{}.wsb", std::process::id()));
    std::fs::write(&bad, format!("<!-- c -->\n{}", EXAMPLE_TEMPLATES[0].1)).unwrap();
    templates.insert("bad".to_string(), bad.clone());
    let mut d = WsbDriver::new(&fake, templates);
    for (name, text) in EXAMPLE_TEMPLATES {
        assert!(d.start(name).is_ok(), "{name}");
        assert_eq!(fake.0.borrow().last().unwrap(), text, "{name}");
    }
    let before = fake.0.borrow().len();
    assert!(d.start("bad").is_err());
    assert_eq!(
        fake.0.borrow().len(),
        before,
        "a refused template reached wsb"
    );
    let _ = std::fs::remove_file(bad);
}

/// Every shipped template keeps the lockdown: no network, no GPU, no clipboard, protected client,
/// nothing writable, no command at logon.
#[test]
fn the_shipped_templates_are_locked_down() {
    for (name, t) in EXAMPLE_TEMPLATES {
        for must in [
            "<Networking>Disable</Networking>",
            "<VGpu>Disable</VGpu>",
            "<ClipboardRedirection>Disable</ClipboardRedirection>",
            "<ProtectedClient>Enable</ProtectedClient>",
        ] {
            assert_eq!(t.matches(must).count(), 1, "{name} lacks {must}");
        }
        assert!(
            !t.contains("<ReadOnly>false"),
            "{name} has a writable folder"
        );
        assert!(
            !t.contains("<LogonCommand"),
            "{name} runs a command at logon"
        );
    }
}
