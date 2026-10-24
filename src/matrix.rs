// SPDX-License-Identifier: MIT OR Apache-2.0
//! The recorded capability matrix: one TSV row per (launcher, capability) test result.
//!
//! The file format and the rules for reading it belong to CapAnt (the same nine columns as
//! PersistAnt, plus optional evidence columns). AntSpawn adds one strictness on top: a
//! `capability` row naming a capability AntSpawn does not define is refused, and so is a duplicate
//! row, because a row that is skipped silently turns a capability off (or hides that it was
//! asserted). **A capability with no row, or a row that says `no`, is off.** Rows of other kinds
//! (for example `measurement`) are kept but never grant anything.

use crate::capability::Capability;

pub use capant::{Matrix, Row};

/// Why a matrix file was refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MatrixError {
    #[error("{0}")]
    Parse(capant::ParseError),
    #[error("{0:?}")]
    Warning(capant::Warning),
    #[error("capability row for `{backend}` names unknown capability `{id}`")]
    UnknownCapability { id: String, backend: String },
}

/// Parse a matrix file. Strict: see the module docs.
pub fn parse(text: &str) -> Result<Matrix, MatrixError> {
    let parsed = capant::parse(text).map_err(MatrixError::Parse)?;
    if let Some(w) = parsed.warnings.into_iter().next() {
        return Err(MatrixError::Warning(w));
    }
    for r in parsed.matrix.rows() {
        if r.subject_kind == "capability" && r.subject.as_str().parse::<Capability>().is_err() {
            return Err(MatrixError::UnknownCapability {
                id: r.subject.to_string(),
                backend: r.backend.clone(),
            });
        }
    }
    Ok(parsed.matrix)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote";

    fn file(rows: &[&str]) -> String {
        let mut s = format!("{HEADER}\n");
        for r in rows {
            s.push_str(r);
            s.push('\n');
        }
        s
    }

    const YES: &str = "capability\tnetwork.off\tnaive\t1\t\tyes\t\tprobe=a\t";
    const NO: &str = "capability\tisolation.no-read-outside\tnaive\t1\t\tno\tread C:\\Projects succeeded\tprobe=b\t";

    #[test]
    fn the_committed_matrix_rewrites_byte_for_byte() {
        let text = include_str!("../matrix/launchers.tsv");
        let m = parse(text).unwrap();
        assert_eq!(capant::write(&m), text.replace("\r\n", "\n"));
    }

    #[test]
    fn round_trips() {
        let text = file(&[YES, NO]);
        assert_eq!(capant::write(&parse(&text).unwrap()), text);
    }

    #[test]
    fn crlf_files_parse() {
        let text = file(&[YES]).replace('\n', "\r\n");
        assert_eq!(parse(&text).unwrap().rows().len(), 1);
    }

    #[test]
    fn malformed_files_are_errors() {
        assert!(matches!(parse(""), Err(MatrixError::Parse(_))));
        assert!(matches!(parse("a\tb\n"), Err(MatrixError::Parse(_))));
        assert!(matches!(
            parse(&file(&["capability\tnetwork.off\tnaive"])),
            Err(MatrixError::Parse(_))
        ));
        assert!(matches!(
            parse(&file(&["capability\tnetwork.off\tnaive\t1\t\tmaybe\t\t\t"])),
            Err(MatrixError::Parse(_))
        ));
        assert!(
            matches!(
                parse(&file(&[
                    "capability\tnetwork.of\tnaive\t1\t\tyes\t\tprobe=a\t"
                ])),
                Err(MatrixError::UnknownCapability { .. })
            ),
            "a typo'd capability is refused, never ignored"
        );
        assert!(matches!(
            parse(&file(&[YES, YES])),
            Err(MatrixError::Warning(_))
        ));
    }

    #[test]
    fn other_kinds_are_kept_but_grant_nothing() {
        let m = parse(&file(&[
            "measurement\tstart-latency-class\tnaive\t1\t\tyes\t\tclass=fast\t",
        ]))
        .unwrap();
        assert_eq!(m.rows().len(), 1);
    }
}
