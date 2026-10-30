// SPDX-License-Identifier: MIT OR Apache-2.0
//! The tail of a job's stdout and stderr, kept so `spawn run` can return it in `--results`
//! (`stdout.tail`, `stderr.tail`) and name what it did not capture in `spawn-result.json`.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// How many trailing bytes of each stream are kept.
pub const TAIL_BYTES: usize = 64 * 1024;

/// Names `spawn run` writes into `--results`; a job may not declare an `[[output]]` with one.
pub const RESERVED_RESULT_NAMES: [&str; 3] = ["stdout.tail", "stderr.tail", "spawn-result.json"];

/// A bounded buffer fed by a reader thread. [`Tail::snapshot`] never waits for end of file, so a
/// descendant that keeps the pipe open cannot stall the run.
#[derive(Clone, Default)]
pub struct Tail(Arc<Mutex<VecDeque<u8>>>);

impl Tail {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read `source` on a thread until it ends, keeping the last [`TAIL_BYTES`].
    pub fn capture(&self, mut source: impl Read + Send + 'static) -> JoinHandle<()> {
        let buf = self.0.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = source.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut b = buf.lock().unwrap_or_else(|p| p.into_inner());
                b.extend(&chunk[..n]);
                let excess = b.len().saturating_sub(TAIL_BYTES);
                b.drain(..excess);
            }
        })
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .copied()
            .collect()
    }
}

/// Give reader threads a moment to drain what the exited job wrote; a thread held open by a
/// descendant is left running after `grace`.
pub fn settle(handles: &mut Vec<JoinHandle<()>>, grace: std::time::Duration) {
    let end = std::time::Instant::now() + grace;
    while !handles.iter().all(JoinHandle::is_finished) && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    handles.retain(|h| !h.is_finished());
}

/// The last [`TAIL_BYTES`] of `bytes`.
pub fn tail_of(bytes: &[u8]) -> Vec<u8> {
    bytes[bytes.len().saturating_sub(TAIL_BYTES)..].to_vec()
}

/// What an environment captured of the job's output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Streams {
    pub stdout: Option<Vec<u8>>,
    pub stderr: Option<Vec<u8>>,
    /// Both streams are in `stdout` (the Windows Sandbox guest redirects `2>&1`).
    pub combined: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_last_bytes() {
        let t = Tail::new();
        let data: Vec<u8> = (0..(TAIL_BYTES * 2 + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        t.capture(std::io::Cursor::new(data.clone()))
            .join()
            .unwrap();
        assert_eq!(t.snapshot(), data[data.len() - TAIL_BYTES..]);
    }

    #[test]
    fn a_short_stream_is_kept_whole() {
        let t = Tail::new();
        t.capture(std::io::Cursor::new(b"hello".to_vec()))
            .join()
            .unwrap();
        assert_eq!(t.snapshot(), b"hello");
        assert_eq!(tail_of(b"abc"), b"abc");
    }
}
