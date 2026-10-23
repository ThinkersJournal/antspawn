// SPDX-License-Identifier: MIT OR Apache-2.0
//! Live proof: fetch the pinned runner archive into a cache dir, verify it, unpack it beside the cache.
//!
//!     cargo run --release --example runner_fetch -- <cache-dir> <unpack-dir>

use std::path::Path;
use std::time::Instant;

use antspawn::adapter::runner_cache::{ensure_archive, unpack, verify_file};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let [cache, out] = a.as_slice() else {
        eprintln!("usage: runner_fetch <cache-dir> <unpack-dir>");
        std::process::exit(2);
    };
    let t = Instant::now();
    let zip = ensure_archive(Path::new(cache)).unwrap_or_else(|e| fail(&e));
    println!(
        "PROOF archive {} ready in {} ms",
        zip.display(),
        t.elapsed().as_millis()
    );
    verify_file(&zip).unwrap_or_else(|e| fail(&e));
    println!("PROOF second verify passed");
    let t = Instant::now();
    unpack(&zip, Path::new(out)).unwrap_or_else(|e| fail(&e));
    println!(
        "PROOF unpacked into {out} in {} ms",
        t.elapsed().as_millis()
    );
    for f in ["run.cmd", "config.cmd", "bin/Runner.Listener.exe"] {
        println!("PROOF {f} present={}", Path::new(out).join(f).exists());
    }
}

fn fail(e: &dyn std::fmt::Display) -> ! {
    eprintln!("FAILED: {e}");
    std::process::exit(1);
}
