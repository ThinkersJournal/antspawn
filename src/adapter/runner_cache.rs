// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fetch, verify and unpack the pinned runner archive (see [`super::runner_pin`]).
//!
//! The archive is hashed as it is read, never trusted by name: a cached file is reused only if it passes
//! [`verify_file`], a download is renamed into place only after it passes, and an unpack refuses an entry
//! that would land outside the destination. Nothing here starts the runner.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use ring::digest::{Context, SHA256};

use super::runner_pin::{self, PinError};

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("runner archive rejected: {0}")]
    Pin(#[from] PinError),
    #[error("download of the runner archive failed: {0}")]
    Download(String),
    #[error("runner archive is not a readable zip: {0}")]
    Zip(String),
    #[error("runner archive entry `{0}` would land outside the destination")]
    Escapes(String),
    #[error("unpack destination `{0}` already exists; refusing to mix archives")]
    DestExists(PathBuf),
}

/// Hash `path` in a stream and compare size and digest to the pin.
pub fn verify_file(path: &Path) -> Result<(), CacheError> {
    verify_reader(File::open(path)?, runner_pin::SIZE, runner_pin::SHA256_HEX)
}

fn verify_reader(mut r: impl Read, size: u64, hex: &str) -> Result<(), CacheError> {
    let mut ctx = Context::new(&SHA256);
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
        total += n as u64;
    }
    runner_pin::check(total, &ctx.finish(), size, hex)?;
    Ok(())
}

/// Path of the verified archive in `cache_dir`, downloading it first if the cache lacks a good copy.
pub fn ensure_archive(cache_dir: &Path) -> Result<PathBuf, CacheError> {
    ensure_archive_from(
        cache_dir,
        runner_pin::URL,
        runner_pin::SIZE,
        runner_pin::SHA256_HEX,
    )
}

fn ensure_archive_from(
    cache_dir: &Path,
    url: &str,
    size: u64,
    hex: &str,
) -> Result<PathBuf, CacheError> {
    fs::create_dir_all(cache_dir)?;
    let dest = cache_dir.join(runner_pin::ARCHIVE);
    if dest.exists() {
        if verify_reader(File::open(&dest)?, size, hex).is_ok() {
            return Ok(dest);
        }
        fs::remove_file(&dest)?; // a cached file that fails the pin is never kept
    }
    let part = cache_dir.join(format!("{}.part", runner_pin::ARCHIVE));
    let result = download(url, &part).and_then(|()| {
        verify_reader(File::open(&part)?, size, hex)?;
        fs::rename(&part, &dest)?;
        Ok(())
    });
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result.map(|()| dest)
}

fn download(url: &str, to: &Path) -> Result<(), CacheError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .header("User-Agent", "antspawn")
        .call()
        .map_err(|e| CacheError::Download(e.to_string()))?;
    let mut out = File::create(to)?;
    io::copy(&mut resp.body_mut().as_reader(), &mut out)
        .map_err(|e| CacheError::Download(e.to_string()))?;
    out.flush()?;
    Ok(())
}

/// Unpack `archive` (already verified by the caller) into `dest`, which must not exist yet.
/// An entry whose path is absolute, has a drive, or climbs out with `..` is refused and nothing is kept.
pub fn unpack(archive: &Path, dest: &Path) -> Result<(), CacheError> {
    if dest.exists() {
        return Err(CacheError::DestExists(dest.to_owned()));
    }
    let result = unpack_into(archive, dest);
    if result.is_err() {
        let _ = fs::remove_dir_all(dest);
    }
    result
}

fn unpack_into(archive: &Path, dest: &Path) -> Result<(), CacheError> {
    let mut zip =
        zip::ZipArchive::new(File::open(archive)?).map_err(|e| CacheError::Zip(e.to_string()))?;
    fs::create_dir_all(dest)?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| CacheError::Zip(e.to_string()))?;
        let name = entry
            .name()
            .map_err(|e| CacheError::Zip(e.to_string()))?
            .into_owned();
        let rel = entry.enclosed_name().ok_or(CacheError::Escapes(name))?;
        let target = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(p) = target.parent() {
                fs::create_dir_all(p)?;
            }
            io::copy(&mut entry, &mut File::create(&target)?)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antspawn-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (n, b) in entries {
            w.start_file(*n, SimpleFileOptions::default()).unwrap();
            w.write_all(b).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn unpack_writes_the_entries_and_refuses_an_existing_destination() {
        let d = tmp("ok");
        let z = d.join("a.zip");
        fs::write(&z, make_zip(&[("run.cmd", b"x"), ("bin/l.exe", b"y")])).unwrap();
        unpack(&z, &d.join("out")).unwrap();
        assert_eq!(fs::read(d.join("out/bin/l.exe")).unwrap(), b"y");
        assert!(matches!(
            unpack(&z, &d.join("out")),
            Err(CacheError::DestExists(_))
        ));
    }

    #[test]
    fn a_path_climbing_entry_is_refused_and_nothing_is_kept() {
        let d = tmp("slip");
        let z = d.join("evil.zip");
        fs::write(
            &z,
            make_zip(&[("fine.txt", b"x"), ("../escaped.txt", b"y")]),
        )
        .unwrap();
        let err = unpack(&z, &d.join("out")).unwrap_err();
        assert!(
            matches!(err, CacheError::Escapes(ref n) if n == "../escaped.txt"),
            "{err}"
        );
        assert!(!d.join("escaped.txt").exists(), "nothing written outside");
        assert!(!d.join("out").exists(), "partial unpack removed");
    }

    #[test]
    fn a_cached_file_that_fails_the_pin_is_replaced_not_reused() {
        let d = tmp("bad-cache");
        let cache = d.join("c");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join(runner_pin::ARCHIVE), b"tampered").unwrap();
        // Unreachable URL: the bad cache must be dropped and the download attempted (and fail).
        let err = ensure_archive_from(&cache, "http://127.0.0.1:1/x.zip", 3, "00").unwrap_err();
        assert!(matches!(err, CacheError::Download(_)), "{err}");
        assert!(!cache.join(runner_pin::ARCHIVE).exists());
        assert!(!cache.join(format!("{}.part", runner_pin::ARCHIVE)).exists());
    }

    #[test]
    fn verify_reader_streams_and_matches_the_vector() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_reader(&b"abc"[..], 3, abc).is_ok());
        assert!(matches!(
            verify_reader(&b"abd"[..], 3, abc),
            Err(CacheError::Pin(PinError::Digest { .. }))
        ));
    }
}
