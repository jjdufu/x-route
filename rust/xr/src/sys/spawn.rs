use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use rustix::process::{setrlimit, setsid, Resource, Rlimit};

use crate::env::Env;

pub fn enter_detached_session() {
    let _ = setsid();
    // Register a no-op handler for SIGHUP so terminal/WebUI hangup does not terminate us.
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGHUP, flag);
}

pub fn unlock_memlock_rlimit() {
    let unlim = Rlimit {
        current: None,
        maximum: None,
    };
    let _ = setrlimit(Resource::Memlock, unlim);
}

pub fn spawn_self_detached(env: &Env, args: &[&str]) -> std::io::Result<Child> {
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

pub fn spawn_self_to_log(env: &Env, args: &[&str], log_path: &Path) -> std::io::Result<Child> {
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
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

pub fn exec_crosvm_trampoline(raw_args: &[String]) -> i32 {
    enter_detached_session();
    unlock_memlock_rlimit();

    let Some((prog, rest)) = raw_args.split_first() else {
        return 2;
    };
    let _err = Command::new(prog).args(rest).exec();
    127
}
