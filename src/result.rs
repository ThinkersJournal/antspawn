// SPDX-License-Identifier: MIT OR Apache-2.0
//! What `spawn run` reports: its exit code and the `status` in `spawn-result.json`.
//!
//! A job's own exit code passes through untouched, so spawn's two failure codes share the number space
//! with it: [`EXIT_REFUSED`] and [`EXIT_ERROR`] are the quick signal (a job that itself exits 125 or 126
//! looks the same), `spawn-result.json` in the results folder is the authoritative one. They sit away
//! from 2 (clap's usage error, and the code `spawn run` itself used before) and from 124 (timeout) and
//! 137 (killed).

use crate::Exit;

/// No launcher is certified (or set up) for the spec's needs: nothing ran.
pub const EXIT_REFUSED: u8 = 125;
/// Anything else on spawn's side: unreadable or invalid spec, copy or cleanup failure.
pub const EXIT_ERROR: u8 = 126;
pub const EXIT_TIMED_OUT: u8 = 124;
pub const EXIT_KILLED: u8 = 137;

/// Why `spawn run` did not run, or did not finish, the job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fail {
    Refused(String),
    Error(String),
}

/// The report for one `spawn run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// `exited`, `timed_out`, `killed`, `refused` or `error`.
    pub status: &'static str,
    pub message: Option<String>,
    pub code: u8,
}

/// `outcome` is what the attempt returned; `ran` the job's exit if it got that far.
pub fn verdict(outcome: &Result<(), Fail>, ran: Option<Exit>) -> Verdict {
    let v = |status, message: Option<&str>, code| Verdict {
        status,
        message: message.map(str::to_owned),
        code,
    };
    match (outcome, ran) {
        (Err(Fail::Refused(m)), _) => v("refused", Some(m), EXIT_REFUSED),
        (Err(Fail::Error(m)), _) => v("error", Some(m), EXIT_ERROR),
        (Ok(()), Some(Exit::Code(c))) => v("exited", None, u8::try_from(c).unwrap_or(1)),
        (Ok(()), Some(Exit::TimedOut)) => v("timed_out", Some("job timed out"), EXIT_TIMED_OUT),
        (Ok(()), Some(Exit::Killed)) => v("killed", None, EXIT_KILLED),
        (Ok(()), None) => v("error", Some("no result"), EXIT_ERROR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_exiting_2_is_an_exit_not_a_refusal() {
        // The negative control: the code spawn used to give a refusal is now a plain job exit.
        let job = verdict(&Ok(()), Some(Exit::Code(2)));
        assert_eq!((job.status, job.code), ("exited", 2));
        let refusal = verdict(&Err(Fail::Refused("none".into())), None);
        assert_eq!((refusal.status, refusal.code), ("refused", EXIT_REFUSED));
        assert_ne!(job.code, refusal.code);
        assert_ne!(job.status, refusal.status);
    }

    #[test]
    fn the_other_outcomes_have_their_own_codes() {
        assert_eq!(verdict(&Ok(()), Some(Exit::Code(0))).code, 0);
        assert_eq!(verdict(&Ok(()), Some(Exit::Code(300))).code, 1);
        assert_eq!(verdict(&Ok(()), Some(Exit::TimedOut)).code, 124);
        assert_eq!(verdict(&Ok(()), Some(Exit::Killed)).code, 137);
        let e = verdict(&Err(Fail::Error("bad".into())), None);
        assert_eq!((e.status, e.code), ("error", EXIT_ERROR));
        assert_ne!(EXIT_ERROR, EXIT_REFUSED);
    }
}
