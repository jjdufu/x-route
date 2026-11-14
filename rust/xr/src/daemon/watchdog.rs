use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use rustix::process::Signal;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;

use crate::daemon;
use crate::daemon::keepalive::{
    cgroup_escape, cgroup_escape_tree, emergency_teardown, stale_gateway, vm_exit_reason,
    vm_guest_reset_pending,
};
use crate::diag;
use crate::env::Env;
use crate::log;
use crate::log::note_ok;
use crate::log::rotate_all_logs;
use crate::sys::fsx;
use crate::sys::send_signal_logged;
use crate::sys::sys_shutting_down;

#[must_use]
pub fn main_loop(env: &Env) -> i32 {
    if let Err(err) = env.ensure_dirs() {
        fsx::record_failure(env, "ensure_dirs", &env.xr_data, &err);
    }
    let run = env.run_dir();
    let pid_s = std::process::id().to_string();
    fsx::write_logged(env, run.join("watchdog.pid"), format!("{pid_s}\n"));
    note_ok(env, "cgroup_escape", cgroup_escape(env, &pid_s));

    let hup = Arc::new(AtomicBool::new(false));
    let term = Arc::new(AtomicBool::new(false));
    if let Err(err) = flag::register(SIGHUP, Arc::clone(&hup)) {
        log::err(env, "daemon", &format!("信号注册失败 SIGHUP: {err}"));
    }
    if let Err(err) = flag::register(SIGTERM, Arc::clone(&term)) {
        log::err(env, "daemon", &format!("信号注册失败 SIGTERM: {err}"));
    }
    if let Err(err) = flag::register(SIGINT, Arc::clone(&term)) {
        log::err(env, "daemon", &format!("信号注册失败 SIGINT: {err}"));
    }

    log::info(env, "watchdog", &format!("已启动 pid={pid_s}"));

    loop {
        if term.load(Ordering::Relaxed) {
            fsx::remove_logged(env, run.join("watchdog.pid"));
            return 0;
        }
        if sys_shutting_down(env) {
            fsx::remove_logged(env, run.join("watchdog.pid"));
            return 0;
        }
        if env.is_module_disabled() {
            thread::sleep(Duration::from_secs(15));
            continue;
        }
        if stale_gateway(env) {
            if daemon::running(env) && vm_guest_reset_pending(env) {
                send_signal_logged(env, &daemon::pid(env), Signal::USR2);
            } else {
                let reason = vm_exit_reason(env);
                emergency_teardown(env, &format!("{reason}（看门狗）"));
            }
        }
        if daemon::running(env) {
            cgroup_escape_tree(env, std::process::id());
        } else {
            log::info(env, "watchdog", "守护进程不在，重新拉起");
            diag::snap_bg(env, "看门狗重新拉起守护进程");
            note_ok(env, "ensure_daemon", daemon::ensure(env));
        }
        rotate_all_logs(env);
        thread::sleep(Duration::from_secs(env.watchdog_interval));
    }
}
