// SPDX-License-Identifier: MIT OR Apache-2.0
//! `--help` / `--version` for the helper binaries, which do not use clap: each calls [`handle`] first.

/// If the only argument is `--help`/`-h` or `--version`/`-V`, prints it to stdout and returns `true`
/// (the caller exits 0). Any other arguments are left for the binary's own parser.
#[must_use]
pub fn handle(name: &str, usage: &str, args: &[String]) -> bool {
    match args {
        [a] if a == "--help" || a == "-h" => {
            println!("{name} {}\n\nusage: {usage}", env!("CARGO_PKG_VERSION"));
            true
        }
        [a] if a == "--version" || a == "-V" => {
            println!("{name} {}", env!("CARGO_PKG_VERSION"));
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::handle;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn only_a_lone_help_or_version_flag_is_handled() {
        for f in ["--help", "-h", "--version", "-V"] {
            assert!(handle("x", "x [opts]", &v(&[f])), "{f}");
        }
        // known-bad controls: anything else is the binary's own business
        assert!(!handle("x", "u", &v(&[])));
        assert!(!handle("x", "u", &v(&["--config", "f"])));
        assert!(!handle("x", "u", &v(&["--help", "extra"])));
    }
}
