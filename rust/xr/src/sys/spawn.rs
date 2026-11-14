use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};

use rustix::process::{Resource, Rlimit, setrlimit, setsid};

use crate::env::Env;
use crate::log::record_setup_failure;

pub(crate) fn enter_detached_session(env: &Env) {
    if let Err(err) = setsid() {
        record_setup_failure(env, "setsid", &err);
    }
    // Register a no-op handler for SIGHUP so terminal/WebUI hangup does not terminate us.
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Err(err) = signal_hook::flag::register(signal_hook::consts::SIGHUP, flag) {
        record_setup_failure(env, "SIGHUP handler", &err);
    }
}

pub(crate) fn unlock_memlock_rlimit(env: &Env) {
    let unlim = Rlimit {
        current: None,
        maximum: None,
    };
    if let Err(err) = setrlimit(Resource::Memlock, unlim) {
        record_setup_failure(env, "setrlimit memlock", &err);
    }
}

/// 以新进程组分离启动本 `xr`，标准输入输出全部丢弃。
///
/// # Errors
///
/// 进程无法创建（可执行文件缺失、资源不足等）时返回 I/O 错误。
pub(crate) fn self_detached(env: &Env, args: &[&str]) -> std::io::Result<Child> {
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

#[must_use]
pub(crate) fn exec_crosvm_trampoline(env: &Env, raw_args: &[String]) -> i32 {
    enter_detached_session(env);
    unlock_memlock_rlimit(env);

    let Some((prog, rest)) = raw_args.split_first() else {
        return 2;
    };
    let _err = Command::new(prog).args(rest).exec();
    127
}
