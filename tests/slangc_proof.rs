// SPDX-License-Identifier: MIT OR Apache-2.0
//! PR4-proof: a real toolchain (`slangc`, Unpopped's shader compiler) runs through each privileged
//! launcher and must produce byte-identical SPIR-V to a direct host run. Runner-only: a launcher
//! that is not set up, or a host without `slangc`, prints `UNVERIFIED` and returns; never
//! `#[ignore]`d. Timings are printed (`MEASURE ...`) for `docs/MEASUREMENTS.md`; record them there
//! with date and ref. `slangc` is found via `SLANGC` or `PATH`; only `slangc.exe` and its
//! `*slang*.dll` neighbours are copied into the mounted tool directory.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use antspawn::launchers::{Account, WindowsSandbox, windows_sandbox::SANDBOX_EXE};
use antspawn::runner::TASK_NAME;
use antspawn::{Exit, JobSpec, Launcher};

const SHADER: &str =
    "[shader(\"compute\")]\n[numthreads(1, 1, 1)]\nvoid main(uint3 id : SV_DispatchThreadID) {}\n";

fn slangc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SLANGC") {
        return Some(PathBuf::from(p));
    }
    let out = Command::new("where").arg("slangc").output().ok()?;
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .to_owned();
    out.status.success().then(|| PathBuf::from(first))
}

/// Copy `slangc.exe` and its `slang*`/`glslang*` neighbours (not debug `*d` DLLs) into `dest`.
fn stage_tools(slangc: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    let from = slangc.parent().unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        let wanted = name == "slangc.exe"
            || ((name.starts_with("slang") || name.starts_with("glslang"))
                && name.ends_with(".dll")
                && !name.ends_with("d.dll"));
        if wanted && e.path().is_file() {
            std::fs::copy(e.path(), dest.join(e.file_name())).unwrap();
        }
    }
    assert!(dest.join("slangc.exe").exists(), "slangc.exe staged");
}

/// `base` is the guest directory for mounts: empty for a launcher with a relative work dir, an
/// absolute path ending in a backslash for a VM, whose mapped folders must be absolute.
fn spec(tools: &Path, shader: &Path, base: &str) -> JobSpec {
    let cwd = match base.strip_suffix('\\') {
        Some(dir) => format!("cwd = '{dir}'\n"),
        None => String::new(),
    };
    JobSpec::from_toml(&format!(
        "[job]\nname = \"slangc\"\n{cwd}cmd = ['{base}tools/slangc.exe', '{base}in/hello.slang', '-target', 'spirv', '-o', '{base}out.spv']\n\
         [limits]\ntime_s = 240\n\
         [[mount]]\nhost = '{}'\nguest = '{base}tools'\nmode = \"ro\"\n\
         [[mount]]\nhost = '{}'\nguest = '{base}in'\nmode = \"ro\"\n\
         [[output]]\nguest = '{base}out.spv'\nname = \"out.spv\"\n",
        tools.display(),
        shader.display()
    ))
    .unwrap()
}

fn through(l: &dyn Launcher, s: &JobSpec, results: &Path) -> (Exit, f64, f64) {
    let t = Instant::now();
    let mut env = l.provision(s).unwrap();
    let provisioned = t.elapsed().as_secs_f64();
    let exit = env.run(s).unwrap();
    env.collect(s, results).unwrap();
    env.destroy().unwrap();
    (exit, provisioned, t.elapsed().as_secs_f64())
}

fn proof(l: &dyn Launcher, set_up: bool, why: &str, base: &str) {
    if !set_up {
        println!("UNVERIFIED: {} is not set up ({why})", l.id());
        return;
    }
    let Some(slangc) = slangc() else {
        println!("UNVERIFIED: no slangc (set SLANGC or put it on PATH)");
        return;
    };
    let root =
        std::env::temp_dir().join(format!("antspawn-slangc-{}-{}", l.id(), std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (tools, shader, results) = (root.join("tools"), root.join("in"), root.join("res"));
    stage_tools(&slangc, &tools);
    std::fs::create_dir_all(&shader).unwrap();
    std::fs::write(shader.join("hello.slang"), SHADER).unwrap();

    // Reference: the same command directly on the host.
    let want = root.join("want.spv");
    let st = Command::new(tools.join("slangc.exe"))
        .current_dir(&root)
        .args(["in/hello.slang", "-target", "spirv", "-o"])
        .arg(&want)
        .status()
        .unwrap();
    assert!(
        st.success(),
        "host slangc must work, or the proof proves nothing"
    );
    let want = std::fs::read(&want).unwrap();
    assert!(!want.is_empty());

    let s = spec(&tools, &shader, base);
    let (exit, provisioned, total) = through(l, &s, &results);
    assert_eq!(exit, Exit::Code(0), "slangc through {}", l.id());
    let got = std::fs::read(results.join("out.spv")).expect("output collected");
    assert_eq!(
        got,
        want,
        "SPIR-V through {} differs from the host's",
        l.id()
    );
    println!(
        "MEASURE launcher={} provision_s={provisioned:.2} total_s={total:.2} spirv_bytes={}",
        l.id(),
        got.len()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn slangc_through_account_matches_the_host() {
    let ok = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .output()
        .is_ok_and(|o| o.status.success());
    proof(
        &Account::default(),
        ok,
        "scripts/account-setup.ps1 -Apply has not run",
        "",
    );
}

#[test]
fn slangc_through_windows_sandbox_matches_the_host() {
    proof(
        &WindowsSandbox,
        Path::new(SANDBOX_EXE).exists(),
        "Windows Sandbox feature is not enabled",
        "C:\\work\\",
    );
}
