// SPDX-License-Identifier: MIT OR Apache-2.0
//! Toolchain loading cost per job in Windows Sandbox:
//! `cargo run --example toolchain [N]` (N rounds per variant, default 3; `SLANGC` or `PATH` finds `slangc`;
//! `PAD_MIB=<n>` adds an unused filler file of n MiB to the toolchain folder, to see how each variant scales).
//!
//! Every job is a fresh VM, so a toolchain cannot stay installed between jobs. Three variants, each
//! `provision + run + collect + destroy` over N sequential rounds, reported as median milliseconds:
//! * `none`: a no-op job, the cost of the VM itself;
//! * `mapped`: `slangc` runs straight from a read-only mapped host folder (the toolchain cache);
//! * `copied`: the job first copies that folder to the guest's own disk, then runs `slangc` from there.
//!
//! The toolchain is `slangc.exe` plus its `slang*`/`glslang*` DLLs and the Visual C++ runtime, the
//! same set `tests/slangc_proof.rs` stages. A host without `slangc` or a launcher that is not set up
//! prints `UNVERIFIED` and exits 0. Prints `MEASURE` lines for `docs/MEASUREMENTS.md`.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    imp::main()
}

#[cfg(not(windows))]
fn main() {
    println!("UNVERIFIED: toolchain loading is measured through Windows Sandbox only");
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::time::{Duration, Instant};

    use antspawn::launchers::WindowsSandbox;
    use antspawn::{Exit, JobSpec, LaunchError, Launcher};

    const SHADER: &str = "[shader(\"compute\")]\n[numthreads(1, 1, 1)]\nvoid main(uint3 id : SV_DispatchThreadID) {}\n";
    const CRT: [&str; 3] = ["msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll"];
    /// Guest working directory: absolute, because a VM's mapped folders must be.
    const BASE: &str = r"C:\w";

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

    /// Copy `slangc.exe`, its `slang*`/`glslang*` DLLs (not debug `*d`) and the VC++ runtime from this
    /// host's System32 into `dest`; returns the bytes staged.
    fn stage_tools(slangc: &Path, dest: &Path) -> u64 {
        std::fs::create_dir_all(dest).expect("tools dir");
        let mut bytes = 0;
        for e in std::fs::read_dir(slangc.parent().expect("slangc dir"))
            .expect("read slangc dir")
            .flatten()
        {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let wanted = name == "slangc.exe"
                || ((name.starts_with("slang") || name.starts_with("glslang"))
                    && name.ends_with(".dll")
                    && !name.ends_with("d.dll"));
            if wanted && e.path().is_file() {
                bytes += std::fs::copy(e.path(), dest.join(e.file_name())).expect("copy tool");
            }
        }
        let sys = Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot")).join("System32");
        for n in CRT {
            bytes += std::fs::copy(sys.join(n), dest.join(n)).expect("copy VC++ runtime");
        }
        assert!(dest.join("slangc.exe").exists(), "slangc.exe staged");
        // Filler the job never reads: a mapped folder pays nothing for it, a copied one pays for every byte.
        // Synthetic on purpose; a real toolchain's unused share varies.
        let pad_mib: u64 = std::env::var("PAD_MIB")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if pad_mib > 0 {
            let block: Vec<u8> = (0..1_048_576u32)
                .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
                .collect();
            let mut f = std::fs::File::create(dest.join("filler.bin")).expect("filler");
            for _ in 0..pad_mib {
                std::io::Write::write_all(&mut f, &block).expect("filler write");
            }
            bytes += pad_mib * 1_048_576;
        }
        // The `copied` variant's job: the launcher refuses `&&` in an argument, so the copy-then-run is a script.
        std::fs::write(
            dest.join("copy-and-run.cmd"),
            format!(
                "@echo off\r\nxcopy {BASE}\\tools {BASE}\\copy /E /I /Q /Y >nul\r\nif errorlevel 1 exit /b 1\r\n\\
                 {BASE}\\copy\\slangc.exe {BASE}\\in\\hello.slang -target spirv -o {BASE}\\out.spv\r\n"
            ),
        )
        .expect("copy-and-run.cmd");
        bytes
    }

    fn spec(variant: &str, tools: &Path, shader: &Path) -> JobSpec {
        let compile =
            |exe: &str| format!(r"{exe} {BASE}\in\hello.slang -target spirv -o {BASE}\out.spv");
        let cmd = match variant {
            "none" => "hostname.exe".to_owned(),
            "mapped" => compile(&format!(r"{BASE}\tools\slangc.exe")).replace(' ', "', '"),
            "copied" => format!(r"cmd.exe', '/c', '{BASE}\tools\copy-and-run.cmd"),
            other => unreachable!("variant {other}"),
        };
        JobSpec::from_toml(&format!(
            "[job]\nname = \"toolchain-{variant}\"\ncwd = '{BASE}'\ncmd = ['{cmd}']\n[limits]\ntime_s = 240\n\
             [[mount]]\nhost = '{}'\nguest = '{BASE}\\tools'\nmode = \"ro\"\n\
             [[mount]]\nhost = '{}'\nguest = '{BASE}\\in'\nmode = \"ro\"\n",
            tools.display(),
            shader.display()
        ))
        .expect("toolchain spec")
    }

    fn one(
        l: &dyn Launcher,
        s: &JobSpec,
        results: &Path,
    ) -> Result<(Duration, Duration), LaunchError> {
        let t = Instant::now();
        let mut env = l.provision(s)?;
        let run_start = Instant::now();
        let exit = env.run(s)?;
        let run = run_start.elapsed();
        // A failed or timed-out job measured the failure, not the load: refuse it.
        if exit != Exit::Code(0) {
            return Err(LaunchError::Other(format!("job did not succeed: {exit:?}")));
        }
        env.collect(s, results)?;
        env.destroy()?;
        Ok((run, t.elapsed()))
    }

    /// `median (min to max)` in milliseconds.
    fn median(mut v: Vec<Duration>) -> String {
        v.sort();
        format!(
            "{} ({} to {})",
            v[v.len() / 2].as_millis(),
            v[0].as_millis(),
            v[v.len() - 1].as_millis()
        )
    }

    pub fn main() -> ExitCode {
        let n: usize = std::env::args()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(3)
            .max(1);
        let Some(slangc) = slangc() else {
            println!("UNVERIFIED: no slangc on SLANGC or PATH");
            return ExitCode::SUCCESS;
        };
        let root = std::env::temp_dir().join(format!("antspawn-toolchain-{}", std::process::id()));
        let tools = root.join("tools");
        let shader = root.join("in");
        std::fs::create_dir_all(&shader).expect("shader dir");
        std::fs::write(shader.join("hello.slang"), SHADER).expect("shader");
        let bytes = stage_tools(&slangc, &tools);
        println!(
            "MEASURE toolchain bytes={bytes} ({:.1} MiB) from {}",
            bytes as f64 / 1048576.0,
            slangc.display()
        );
        let l = WindowsSandbox;
        // Round-robin the variants so host drift between rounds lands on all of them alike.
        let variants = ["none", "mapped", "copied"];
        let mut runs = [const { Vec::new() }; 3];
        let mut totals = [const { Vec::new() }; 3];
        for i in 0..n {
            for (v, name) in variants.iter().enumerate() {
                let s = spec(name, &tools, &shader);
                match one(&l, &s, &root.join(format!("{name}{i}"))) {
                    Ok((run, total)) => {
                        runs[v].push(run);
                        totals[v].push(total);
                    }
                    Err(LaunchError::NotProvisioned { launcher, what }) => {
                        println!("UNVERIFIED: launcher {launcher} is not set up: {what}");
                        let _ = std::fs::remove_dir_all(&root);
                        return ExitCode::SUCCESS;
                    }
                    Err(e) => {
                        println!("MEASURE variant={name} round {i} failed: {e}");
                        let _ = std::fs::remove_dir_all(&root);
                        return ExitCode::from(1);
                    }
                }
            }
        }
        for (v, name) in variants.iter().enumerate() {
            println!(
                "MEASURE variant={name} rounds={n} ms median (min to max) run={} total={}",
                median(runs[v].clone()),
                median(totals[v].clone())
            );
        }
        let _ = std::fs::remove_dir_all(&root);
        ExitCode::SUCCESS
    }
}
