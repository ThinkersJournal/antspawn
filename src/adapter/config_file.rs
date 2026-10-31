// SPDX-License-Identifier: MIT OR Apache-2.0
//! Loads the adapter's configuration through ConfigurAnt (`docs/ADAPTER-DECISIONS.md`, decision 1, ruled B).
//!
//! The schema is `configurant.toml` at the repository root, embedded here, so the binary and
//! `configurant check configurant.toml` read the same text. The configuration FILE is plain TOML in the
//! shape [`AdapterConfig::from_toml`] already accepts; what ConfigurAnt adds is a named, row-indexed error
//! with a did-you-mean for an unknown key, and `explain`. The rules a schema cannot say (unique group names
//! and label sets, exact `owner/name` repos, a launcher per group) stay in [`AdapterConfig::validate`], which
//! runs on the loaded rows. The environment is deliberately not passed to the loader: nothing in this
//! schema reads it.

use std::path::PathBuf;

use configurant::{Config, Loader, Row, Schema, Value};

use super::github::{AdapterConfig, ConfigError, RunnerGroup};

/// The schema text (`configurant.toml`).
pub const SCHEMA: &str = include_str!("../../configurant.toml");

fn bad(why: impl std::fmt::Display) -> ConfigError {
    ConfigError::Parse(why.to_string())
}

fn field_string(row: &Row, key: &str) -> Result<String, ConfigError> {
    match row.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(bad(format!("group row has no text `{key}`"))),
    }
}

fn field_list(row: &Row, key: &str) -> Result<Vec<String>, ConfigError> {
    match row.get(key) {
        Some(Value::List(l)) => Ok(l.clone()),
        _ => Err(bad(format!("group row has no list `{key}`"))),
    }
}

fn positive(config: &Config, key: &str) -> Result<u64, ConfigError> {
    let n: i64 = config.get(key).map_err(bad)?;
    u64::try_from(n).map_err(|_| bad(format!("{key} must not be negative")))
}

/// Load `text` (named `origin` in errors) against [`SCHEMA`] and validate the result.
pub fn load(text: &str, origin: &str) -> Result<AdapterConfig, ConfigError> {
    let schema = Schema::parse(SCHEMA).map_err(bad)?;
    let config = Loader::new(&schema)
        .file(origin, text)
        .build()
        .map_err(bad)?;
    let groups = config
        .rows("group")
        .map_err(bad)?
        .iter()
        .map(|row| {
            Ok(RunnerGroup {
                name: field_string(row, "name")?,
                repos: field_list(row, "repos")?,
                labels: field_list(row, "labels")?,
                launcher: field_string(row, "launcher")?,
            })
        })
        .collect::<Result<Vec<_>, ConfigError>>()?;
    let cfg = AdapterConfig {
        org: config.get::<String>("org").map_err(bad)?,
        app_id: positive(&config, "app_id")?,
        installation_id: positive(&config, "installation_id")?,
        key_path: config.get::<PathBuf>("key_path").map_err(bad)?,
        job_timeout_s: positive(&config, "job_timeout_s")?,
        groups,
    };
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

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

[[group]]
name = "antspawn-sandbox"
repos = ["ThinkersJournal/antspawn", "ThinkersJournal/other"]
labels = ["antspawn", "windows-sandbox"]
launcher = "windows-sandbox"
"#;

    fn refused(text: &str) -> String {
        load(text, "adapter.toml")
            .expect_err("must be refused")
            .to_string()
    }

    #[test]
    fn the_schema_is_a_valid_configurant_schema() {
        Schema::parse(SCHEMA).expect("configurant.toml parses");
    }

    #[test]
    fn it_loads_the_same_config_the_strict_serde_parser_does() {
        // The serde parser is the oracle: if the schema drifts from `AdapterConfig`, this fails.
        assert_eq!(
            load(CFG, "adapter.toml").unwrap(),
            AdapterConfig::from_toml(CFG).unwrap()
        );
    }

    #[test]
    fn a_misspelt_key_in_a_group_is_refused_with_the_row_and_a_suggestion() {
        let e = refused(&CFG.replace(
            "launcher = \"windows-sandbox\"",
            "launcher = \"windows-sandbox\"\nlauncer = \"x\"",
        ));
        assert!(e.contains("group[1].launcer"), "{e}");
        assert!(e.contains("launcher"), "{e}");
    }

    #[test]
    fn a_missing_required_key_is_refused_by_name() {
        let e = refused(&CFG.replace("org = \"ThinkersJournal\"\n", ""));
        assert!(e.contains("org"), "{e}");
        let e = refused(&CFG.replacen("launcher = \"account\"\n", "", 1));
        assert!(e.contains("group[0].launcher"), "{e}");
    }

    #[test]
    fn the_timeout_range_is_the_schemas_and_matches_the_constant() {
        let at = |n: u64| CFG.replace("job_timeout_s = 3600", &format!("job_timeout_s = {n}"));
        assert!(load(&at(super::super::github::MAX_JOB_TIME_S), "a.toml").is_ok());
        assert!(load(&at(super::super::github::MAX_JOB_TIME_S + 1), "a.toml").is_err());
        assert!(load(&at(0), "a.toml").is_err());
    }

    #[test]
    fn the_rules_a_schema_cannot_say_still_apply_to_the_loaded_rows() {
        // Control: each of these loads fine in ConfigurAnt (it is well-typed) and is refused by validate.
        let same_labels = CFG.replace(
            "labels = [\"antspawn\", \"windows-sandbox\"]",
            "labels = [\"antspawn\", \"account\"]",
        );
        assert!(refused(&same_labels).contains("label set"));
        let wildcard = CFG.replace("ThinkersJournal/other", "ThinkersJournal/*");
        assert!(load(&wildcard, "a.toml").is_err());
        let no_groups = CFG.split("[[group]]").next().unwrap();
        assert!(load(no_groups, "a.toml").is_err());
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused_by_name() {
        let e = refused(&CFG.replace("app_id = 1", "app_id = \"one\""));
        assert!(e.contains("app_id"), "{e}");
    }
}
