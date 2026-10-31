// SPDX-License-Identifier: MIT OR Apache-2.0
//! `spawn-runner`: the service side of the `account` launcher. It runs AS the restricted account
//! (via the scheduled task `AntSpawnRunner`), never as the launcher's user.
//!
//! * `spawn-runner serve`: run every pending job in the spool, one at a time, then exit. For
//!   each job it starts `spawn-runner exec <id>` as a child, waits, wipes the job's work dir and
//!   only then publishes `results/<id>/exit.code`, so that file's presence means the job's whole
//!   process tree is dead and its work dir is gone.
//! * `spawn-runner exec <id>`: put itself in a kill-on-close Job Object, copy the job in, run the
//!   command (its children inherit the job), copy declared outputs out, write `result.tmp`, then
//!   terminate the job, which kills every survivor and this process with them.
//!
//! The layout root is `ANTSPAWN_ROOT` or `%ProgramData%\antspawn` (see `antspawn::runner`).

use antspawn::runner::{Layout, RunnerConfig, check_spec, copy_tree, encode_exit, valid_job_id};
use antspawn::streams::{Tail, settle};
use antspawn::{Exit, JobSpec};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let layout = Layout::from_env();
    let r = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["serve"] => serve(&layout),
        ["exec", id] if valid_job_id(id) => exec(&layout, id),
        _ => Err("usage: spawn-runner serve | exec <job-id>".into()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spawn-runner: {e}");
            ExitCode::from(2)
        }
    }
}

fn pending(layout: &Layout) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(layout.spool())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|id| valid_job_id(id))
        .filter(|id| layout.job_spool(id).join("spec.toml").is_file())
        .filter(|id| !layout.job_results(id).join("exit.code").exists())
        .collect();
    ids.sort();
    ids
}

fn serve(layout: &Layout) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    loop {
        let ids = pending(layout);
        if ids.is_empty() {
            return Ok(());
        }
        for id in ids {
            let results = layout.job_results(&id);
            std::fs::create_dir_all(&results).map_err(|e| format!("{}: {e}", results.display()))?;
            let status = Command::new(&me).args(["exec", &id]).status();
            // Only now is every process of the job dead; wipe, then publish.
            let _ = std::fs::remove_dir_all(layout.job_work(&id));
            let tmp = results.join("result.tmp");
            if !tmp.exists() {
                let why = format!("exec ended without a result ({status:?})");
                let _ = std::fs::write(results.join("error.txt"), why);
                std::fs::write(&tmp, encode_exit(Exit::Killed)).map_err(|e| e.to_string())?;
            }
            std::fs::rename(&tmp, results.join("exit.code")).map_err(|e| e.to_string())?;
        }
    }
}

fn exec(layout: &Layout, id: &str) -> Result<(), String> {
    let results = layout.job_results(id);
    let fail = |msg: String| {
        let _ = std::fs::write(results.join("error.txt"), &msg);
        msg
    };
    let cfg_text = std::fs::read_to_string(layout.config_file())
        .map_err(|e| fail(format!("runner.toml: {e}")))?;
    let cfg = RunnerConfig::from_toml(&cfg_text).map_err(fail)?;
    let spool = layout.job_spool(id);
    let text = std::fs::read_to_string(spool.join("spec.toml"))
        .map_err(|e| fail(format!("spec.toml: {e}")))?;
    let spec = JobSpec::from_toml(&text).map_err(|e| fail(e.to_string()))?;
    check_spec(&spec, &cfg).map_err(|e| fail(format!("refused: {e}")))?;

    let work = layout.job_work(id);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| fail(e.to_string()))?;
    for (i, m) in spec.mounts.iter().enumerate() {
        let to = work.join(&m.guest);
        let r = match m.mode {
            antspawn::spec::MountMode::Ro => copy_tree(&spool.join("in").join(i.to_string()), &to),
            _ => std::fs::create_dir_all(&to),
        };
        r.map_err(|e| fail(format!("mount `{}`: {e}", m.guest)))?;
    }

    let job = sandbox::contain(spec.limits.ram_mb).map_err(|e| fail(format!("job object: {e}")))?;

    let cwd = spec
        .job
        .cwd
        .as_deref()
        .map_or(work.clone(), |c| work.join(c));
    let limit = Duration::from_secs(cfg.time_limit_s(&spec));
    // A program copied into the work dir is found there; anything else is looked up on PATH.
    let program = {
        let inside = work.join(&spec.job.cmd[0]);
        if antspawn::runner::safe_relative(&spec.job.cmd[0]) && inside.is_file() {
            inside
        } else {
            spec.job.cmd[0].clone().into()
        }
    };
    let (out_tail, err_tail) = (Tail::new(), Tail::new());
    let mut readers = Vec::new();
    let exit = match Command::new(program)
        .args(&spec.job.cmd[1..])
        .current_dir(cwd)
        .envs(&spec.job.env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            if let Some(p) = child.stdout.take() {
                readers.push(out_tail.capture(p));
            }
            if let Some(p) = child.stderr.take() {
                readers.push(err_tail.capture(p));
            }
            wait(&mut child, limit)
        }
        Err(e) => {
            fail(format!("cannot start `{}`: {e}", spec.job.cmd[0]));
            Exit::Code(127)
        }
    };

    settle(&mut readers, Duration::from_millis(500));
    let streams = results.join("streams");
    let _ = std::fs::create_dir_all(&streams);
    let _ = std::fs::write(streams.join("stdout.tail"), out_tail.snapshot());
    let _ = std::fs::write(streams.join("stderr.tail"), err_tail.snapshot());

    let out = results.join("out");
    for o in &spec.outputs {
        let from = work.join(&o.guest);
        if from.exists() {
            copy_tree(&from, &out.join(&o.name))
                .map_err(|e| fail(format!("output `{}`: {e}", o.name)))?;
        }
    }
    std::fs::write(results.join("result.tmp"), encode_exit(exit))
        .map_err(|e| fail(e.to_string()))?;
    // Kills every process in the job, this one included.
    sandbox::terminate(job);
    Ok(())
}

fn wait(child: &mut std::process::Child, limit: Duration) -> Exit {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.code().map_or(Exit::Killed, Exit::Code),
            Ok(None) => {}
            Err(_) => return Exit::Killed,
        }
        if start.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            return Exit::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(windows)]
mod sandbox {
    //! The only `unsafe` in AntSpawn: the Win32 Job Object calls.
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub struct Job(HANDLE);

    /// Create a kill-on-close job (with a per-process memory ceiling if asked) and join it.
    /// Processes started afterwards inherit it.
    pub fn contain(ram_mb: Option<u64>) -> Result<Job, String> {
        // SAFETY: plain Win32 calls with valid pointers to locals that outlive each call; the
        // handle is checked before use and owned by `Job` for the life of the process.
        unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return Err(format!(
                    "CreateJobObjectW: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if let Some(mb) = ram_mb {
                info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
                info.ProcessMemoryLimit =
                    usize::try_from(mb.saturating_mul(1 << 20)).unwrap_or(usize::MAX);
            }
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                return Err(format!(
                    "SetInformationJobObject: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if AssignProcessToJobObject(h, GetCurrentProcess()) == 0 {
                return Err(format!(
                    "AssignProcessToJobObject: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Job(h))
        }
    }

    pub fn terminate(job: Job) {
        // SAFETY: `job.0` is the live handle `contain` returned.
        unsafe {
            TerminateJobObject(job.0, 1);
        }
    }
}

#[cfg(not(windows))]
mod sandbox {
    //! The runner is Windows-only; elsewhere it refuses to run a job rather than run it uncontained.
    pub struct Job;
    pub fn contain(_: Option<u64>) -> Result<Job, String> {
        Err("spawn-runner containment is implemented for Windows only".into())
    }
    pub fn terminate(_: Job) {}
}
