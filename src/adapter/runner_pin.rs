// SPDX-License-Identifier: MIT OR Apache-2.0
//! The pinned `actions/runner` release the hand-off will start inside an environment.
//!
//! Only the pin and its verification live here: the archive is never fetched by this module, and
//! nothing is unpacked or run on the strength of a download that did not match. Provenance, licence
//! and how the digest was checked are in `docs/RUNNER-PROVENANCE.md`.

use ring::digest::{SHA256, digest};

/// The release tag, a commit in `github.com/actions/runner`.
pub const VERSION: &str = "2.338.1";
/// The archive name inside the release.
pub const ARCHIVE: &str = "actions-runner-win-x64-2.338.1.zip";
/// Where the release publishes it.
pub const URL: &str = "https://github.com/actions/runner/releases/download/v2.338.1/actions-runner-win-x64-2.338.1.zip";
/// SHA-256 of the archive, lower-case hex, from the release notes.
pub const SHA256_HEX: &str = "478f533ac2c349040fbaea7e158f7f7d0235085cf35bc81f661af0c715207ff9";
/// Size of the archive in bytes, from the release asset.
pub const SIZE: u64 = 104_210_469;

/// The archive's bytes do not match the pin.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    #[error("runner archive is {got} bytes, the pin says {SIZE}")]
    Size { got: u64 },
    #[error("runner archive sha256 is {got}, the pin says {SHA256_HEX}")]
    Digest { got: String },
}

/// Check `bytes` against the pin: size first, then digest.
pub fn verify(bytes: &[u8]) -> Result<(), PinError> {
    verify_against(bytes, SIZE, SHA256_HEX)
}

fn verify_against(bytes: &[u8], size: u64, hex: &str) -> Result<(), PinError> {
    if bytes.len() as u64 != size {
        return Err(PinError::Size {
            got: bytes.len() as u64,
        });
    }
    let got = hex_of(digest(&SHA256, bytes).as_ref());
    if got != hex {
        return Err(PinError::Digest { got });
    }
    Ok(())
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // SHA-256("abc"), a published test vector.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn matching_bytes_pass() {
        assert_eq!(verify_against(b"abc", 3, ABC), Ok(()));
    }

    #[test]
    fn one_changed_byte_fails_the_digest() {
        let err = verify_against(b"abd", 3, ABC).unwrap_err();
        assert!(matches!(err, PinError::Digest { .. }), "{err}");
    }

    #[test]
    fn wrong_length_fails_before_hashing() {
        assert_eq!(
            verify_against(b"abcd", 3, ABC),
            Err(PinError::Size { got: 4 })
        );
    }

    #[test]
    fn the_pin_is_well_formed() {
        assert_eq!(SHA256_HEX.len(), 64);
        assert!(
            SHA256_HEX
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert!(URL.contains(VERSION) && URL.ends_with(ARCHIVE));
        assert!(
            verify(b"not the runner").is_err(),
            "the real pin refuses other bytes"
        );
    }
}
