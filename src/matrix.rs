// SPDX-License-Identifier: MIT OR Apache-2.0
//! The recorded capability matrix: one TSV row per (launcher, capability) test result.
//!
//! The format is PersistAnt's (same nine columns, same header), so one reader serves both:
//! `subject_kind subject backend version config enabled reason metadata note`. A capability row has
//! `subject_kind = capability`, `subject` = the capability id, `backend` = the launcher id.
//! **A capability with no row, or a row that says `no`, is off.** Rows of other kinds (for
//! example `measurement`) are kept but never grant anything.

use std::collections::HashSet;

use crate::capability::Capability;

/// The exact header line.
pub const HEADER: &str =
    "subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote";

/// One row of the matrix file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub subject_kind: String,
    pub subject: String,
    pub backend: String,
    pub version: String,
    pub config: String,
    pub enabled: bool,
    pub reason: String,
    pub metadata: String,
    pub note: String,
}

/// A parsed matrix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Matrix {
    rows: Vec<Row>,
}

/// Why a matrix file was refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MatrixError {
    #[error("matrix is empty (no header line)")]
    Empty,
    #[error("matrix header is not the expected nine columns")]
    BadHeader,
    #[error("line {line}: expected 9 tab-separated fields, found {found}")]
    FieldCount { line: usize, found: usize },
    #[error("line {line}: `enabled` must be `yes` or `no`, found `{found}`")]
    BadEnabled { line: usize, found: String },
    #[error("line {line}: capability row names unknown capability `{id}`")]
    UnknownCapability { line: usize, id: String },
    #[error("line {line}: duplicate row for {kind} `{subject}` on `{backend}`")]
    Duplicate {
        line: usize,
        kind: String,
        subject: String,
        backend: String,
    },
}

impl Matrix {
    /// Parse a matrix file. Strict: a malformed row is an error, never a skipped row, because a
    /// skipped row silently turns a capability off (or hides that it was asserted).
    pub fn parse(text: &str) -> Result<Self, MatrixError> {
        let mut lines = text.lines().enumerate();
        let (_, header) = lines.next().ok_or(MatrixError::Empty)?;
        if header.trim_end_matches('\r') != HEADER {
            return Err(MatrixError::BadHeader);
        }
        let mut rows = Vec::new();
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        for (i, raw) in lines {
            let line = i + 1;
            let raw = raw.trim_end_matches('\r');
            if raw.is_empty() {
                continue;
            }
            let f: Vec<&str> = raw.split('\t').collect();
            if f.len() != 9 {
                return Err(MatrixError::FieldCount {
                    line,
                    found: f.len(),
                });
            }
            let enabled = match f[5] {
                "yes" => true,
                "no" => false,
                other => {
                    return Err(MatrixError::BadEnabled {
                        line,
                        found: other.to_owned(),
                    });
                }
            };
            if f[0] == "capability" && f[1].parse::<Capability>().is_err() {
                return Err(MatrixError::UnknownCapability {
                    line,
                    id: f[1].to_owned(),
                });
            }
            if !seen.insert((f[0].to_owned(), f[1].to_owned(), f[2].to_owned())) {
                return Err(MatrixError::Duplicate {
                    line,
                    kind: f[0].to_owned(),
                    subject: f[1].to_owned(),
                    backend: f[2].to_owned(),
                });
            }
            rows.push(Row {
                subject_kind: f[0].to_owned(),
                subject: f[1].to_owned(),
                backend: f[2].to_owned(),
                version: f[3].to_owned(),
                config: f[4].to_owned(),
                enabled,
                reason: f[6].to_owned(),
                metadata: f[7].to_owned(),
                note: f[8].to_owned(),
            });
        }
        Ok(Matrix { rows })
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Is `cap` certified for `launcher`? Off unless a capability row says `yes`.
    pub fn enabled(&self, launcher: &str, cap: Capability) -> bool {
        self.rows.iter().any(|r| {
            r.subject_kind == "capability"
                && r.backend == launcher
                && r.subject == cap.id()
                && r.enabled
        })
    }

    /// Write the file back in the canonical form (`parse(to_tsv(m)) == m`).
    pub fn to_tsv(&self) -> String {
        let mut out = String::from(HEADER);
        out.push('\n');
        for r in &self.rows {
            let enabled = if r.enabled { "yes" } else { "no" };
            out.push_str(
                &[
                    r.subject_kind.as_str(),
                    &r.subject,
                    &r.backend,
                    &r.version,
                    &r.config,
                    enabled,
                    &r.reason,
                    &r.metadata,
                    &r.note,
                ]
                .join("\t"),
            );
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rows: &[&str]) -> String {
        let mut s = format!("{HEADER}\n");
        for r in rows {
            s.push_str(r);
            s.push('\n');
        }
        s
    }

    const YES: &str = "capability\tnetwork.off\tnaive\t1\t\tyes\t\ttest=a\t";
    const NO: &str = "capability\tisolation.no-read-outside\tnaive\t1\t\tno\tread C:\\Projects succeeded\ttest=b\t";

    #[test]
    fn a_capability_is_on_only_when_its_row_says_yes() {
        let m = Matrix::parse(&file(&[YES, NO])).unwrap();
        assert!(m.enabled("naive", Capability::NetworkOff));
        assert!(
            !m.enabled("naive", Capability::NoReadOutside),
            "a `no` row grants nothing"
        );
        assert!(
            !m.enabled("naive", Capability::GpuCuda),
            "no row grants nothing"
        );
        assert!(
            !m.enabled("account", Capability::NetworkOff),
            "another launcher's row grants nothing"
        );
    }

    #[test]
    fn round_trips() {
        let text = file(&[YES, NO]);
        let m = Matrix::parse(&text).unwrap();
        assert_eq!(m.to_tsv(), text);
        assert_eq!(Matrix::parse(&m.to_tsv()).unwrap(), m);
    }

    #[test]
    fn crlf_files_parse() {
        let text = file(&[YES]).replace('\n', "\r\n");
        assert!(
            Matrix::parse(&text)
                .unwrap()
                .enabled("naive", Capability::NetworkOff)
        );
    }

    #[test]
    fn malformed_files_are_errors() {
        assert_eq!(Matrix::parse(""), Err(MatrixError::Empty));
        assert_eq!(Matrix::parse("a\tb\n"), Err(MatrixError::BadHeader));
        assert!(matches!(
            Matrix::parse(&file(&["capability\tnetwork.off\tnaive"])),
            Err(MatrixError::FieldCount { line: 2, found: 3 })
        ));
        assert!(matches!(
            Matrix::parse(&file(&["capability\tnetwork.off\tnaive\t1\t\tmaybe\t\t\t"])),
            Err(MatrixError::BadEnabled { .. })
        ));
        assert!(matches!(
            Matrix::parse(&file(&["capability\tnetwork.of\tnaive\t1\t\tyes\t\t\t"])),
            Err(MatrixError::UnknownCapability { .. })
        ));
        assert!(matches!(
            Matrix::parse(&file(&[YES, YES])),
            Err(MatrixError::Duplicate { line: 3, .. })
        ));
    }

    #[test]
    fn other_kinds_are_kept_but_grant_nothing() {
        let m = Matrix::parse(&file(&[
            "measurement\tstart-latency-class\tnaive\t1\t\tyes\t\tclass=fast\t",
        ]))
        .unwrap();
        assert_eq!(m.rows().len(), 1);
        assert!(Capability::ALL.iter().all(|&c| !m.enabled("naive", c)));
    }
}
