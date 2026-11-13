use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;

use crate::daemon::keepalive::{
    cgroup_escape, cgroup_escape_tree, emergency_teardown, stale_gateway, vm_exit_reason,
};
use crate::daemon::{daemon_running, ensure_daemon};
use crate::diag::diag_snap_bg;
use crate::env::Env;
use crate::log::{log_info, rotate_all_logs};
use crate::sys::sys_shutting_down;

pub fn watchdog_loop(env: &Env) -> i32 {
    let _ = env.ensure_dirs();
    let run = env.run_dir();
    let pid_s = std::process::id().to_string();
    let _ = fs::write(run.join("watchdog.pid"), format!("{pid_s}\n"));
    let _ = cgroup_escape(env, &pid_s);

    let hup = Arc::new(AtomicBool::new(false));
    let term = Arc::new(AtomicBool::new(false));
    let _ = flag::register(SIGHUP, Arc::clone(&hup));
    let _ = flag::register(SIGTERM, Arc::clone(&term));
    let _ = flag::register(SIGINT, Arc::clone(&term));

    log_info(env, "watchdog", &format!("已启动 pid={pid_s}"));

    loop {
        if term.load(Ordering::Relaxed) {
            let _ = fs::remove_file(run.join("watchdog.pid"));
            return 0;
        }
        if sys_shutting_down(env) {
            let _ = fs::remove_file(run.join("watchdog.pid"));
            return 0;
        }
        if env.is_module_disabled() {
            thread::sleep(Duration::from_secs(15));
            continue;
        }
        if stale_gateway(env) {
            let reason = vm_exit_reason(env);
            emergency_teardown(env, &format!("{reason}（看门狗）"));
        }
        if daemon_running(env) {
            cgroup_escape_tree(env, std::process::id());
        } else {
            log_info(env, "watchdog", "守护进程不在，重新拉起");
            diag_snap_bg(env, "看门狗重新拉起守护进程");
            let _ = ensure_daemon(env);
        }
        rotate_all_logs(env);
        thread::sleep(Duration::from_secs(env.watchdog_interval));
    }
}
