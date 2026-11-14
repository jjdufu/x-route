use std::fs::OpenOptions;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use rustix::process::{Resource, Rlimit, setrlimit, setsid};

use crate::env::Env;
use crate::log::record_setup_failure;
use crate::sys::fsx;

pub fn enter_detached_session(env: &Env) {
    if let Err(err) = setsid() {
        record_setup_failure(env, "setsid", &err);
    }
    // Register a no-op handler for SIGHUP so terminal/WebUI hangup does not terminate us.
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Err(err) = signal_hook::flag::register(signal_hook::consts::SIGHUP, flag) {
        record_setup_failure(env, "SIGHUP handler", &err);
    }
}

pub fn unlock_memlock_rlimit(env: &Env) {
    let unlim = Rlimit {
        current: None,
        maximum: None,
    };
    if let Err(err) = setrlimit(Resource::Memlock, unlim) {
        record_setup_failure(env, "setrlimit memlock", &err);
    }
}

pub fn self_detached(env: &Env, args: &[&str]) -> std::io::Result<Child> {
    let exe = env.self_exe();
    Command::new(exe)
        .args(args)
        .env("MODDIR", &env.moddir)
        .env("XR_DATA", &env.xr_data)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
}

pub fn self_to_log(env: &Env, args: &[&str], log_path: &Path) -> std::io::Result<Child> {
    if let Some(parent) = log_path.parent() {
        fsx::mkdirs_logged(env, parent);
    }
    let out_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let err_file = out_file.try_clone()?;
    let exe = env.self_exe();
    Command::new(exe)
        .args(args)
        .env("MODDIR", &env.moddir)
        .env("XR_DATA", &env.xr_data)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        .process_group(0)
        .spawn()
}

#[must_use]
pub fn exec_crosvm_trampoline(env: &Env, raw_args: &[String]) -> i32 {
    enter_detached_session(env);
    unlock_memlock_rlimit(env);

    let Some((prog, rest)) = raw_args.split_first() else {
        return 2;
    };
    let _err = Command::new(prog).args(rest).exec();
    127
}
