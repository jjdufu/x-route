pub mod keepalive;
pub mod watchdog;

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rustix::process::Signal;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGUSR1, SIGUSR2};
use signal_hook::flag;

use crate::config::Config;
use crate::daemon::keepalive::{
    cgroup_escape, cgroup_escape_tree, cgroup_path, emergency_teardown, notify_cancel,
    notify_update, routes_emergency_down, stale_gateway, vm_exit_reason, vm_guest_reset_pending,
    vm_stopping, xr_vm_alive,
};
use crate::env::Env;
use crate::log::{debug_expire, log_err, log_info, rotate_all_logs, unix_now};
use crate::net::common::NetCtx;
use crate::sys::spawn::spawn_self_detached;
use crate::sys::{is_pid_running, proc_cmdline, send_signal, shutdown_note, sys_shutting_down};
use crate::traffic::acct_snapshot;
use crate::vm::{run_net_hook, vm_running, vm_start_exec, vm_stop_exec};

fn handle_guest_reboot(env: &Env, reset_times: &mut Vec<u64>) -> bool {
    if !vm_guest_reset_pending(env) {
        return false;
    }
    let now = unix_now();
    reset_times.retain(|&ts| now.saturating_sub(ts) < 120);
    if reset_times.len() >= 3 {
        log_err(env, "vm", "短时间内连续多次检测到客机重启，停止自动拉起");
        return false;
    }
    reset_times.push(now);

    let run = env.run_dir();
    log_info(env, "vm", "检测到 OpenWrt 重启请求，正在重新拉起服务");
    let _ = fs::write(run.join("stopping"), format!("{}\n", std::process::id()));
    if let Ok(wp) = fs::read_to_string(run.join("fuse_watch.pid")) {
        let wp = wp.trim();
        if !wp.is_empty() && is_pid_running(env, wp) {
            let _ = send_signal(wp, Signal::TERM);
        }
    }
    crate::clash::stop_clash_mux(env);
    for p in [
        run.join("crosvm.pid"),
        run.join("crosvm.sock"),
        run.join("state"),
        run.join("ready_at"),
        run.join("fuse_watch.pid"),
        run.join("share.mode"),
        run.join("guest/guest/openclash.status"),
        run.join("last_error"),
        env.fuse_file(),
        env.boot_log(),
    ] {
        let _ = fs::remove_file(p);
    }
    let _ = run_net_hook(env, "down");
    let _ = fs::remove_file(run.join("stopping"));

    let _ = vm_start_exec(env);
    let _ = fs::write(run.join("notify.force"), "");
    notify_update(env);
    true
}

pub fn daemon_pid(env: &Env) -> String {
    fs::read_to_string(env.run_dir().join("daemon.pid"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub fn daemon_running(env: &Env) -> bool {
    let p = daemon_pid(env);
    if !is_pid_running(env, &p) {
        return false;
    }
    if let Some(cmd) = proc_cmdline(env, &p) {
        return cmd.contains("daemon") || cmd.contains("xr");
    }
    true
}

pub fn ensure_watchdog(env: &Env) -> bool {
    if env.in_watchdog || env.daemon_foreground {
        return true;
    }
    let run = env.run_dir();
    let wp = fs::read_to_string(run.join("watchdog.pid"))
        .unwrap_or_default()
        .trim()
        .to_string();
    if is_pid_running(env, &wp) {
        if let Some(cmd) = proc_cmdline(env, &wp) {
            if cmd.contains("watchdog") || cmd.contains("xr") {
                let _ = cgroup_escape(env, &wp);
                return true;
            }
        }
    }
    if spawn_self_detached(env, &["internal", "watchdog"]).is_err() {
        log_err(env, "daemon", "看门狗未能启动");
        return false;
    }
    for _ in 0..8 {
        let p = fs::read_to_string(run.join("watchdog.pid"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if is_pid_running(env, &p) {
            let _ = cgroup_escape(env, &p);
            return true;
        }
        thread::sleep(Duration::from_secs(1));
    }
    log_err(env, "daemon", "看门狗未能启动");
    false
}

pub fn ensure_daemon(env: &Env) -> bool {
    if env.daemon_foreground {
        return true;
    }
    let _ = env.ensure_dirs();
    if daemon_running(env) {
        cgroup_escape_tree(env, std::process::id());
        let _ = ensure_watchdog(env);
        return true;
    }
    let run = env.run_dir();
    let _ = fs::remove_file(run.join("daemon.ready"));
    let _ = fs::remove_file(run.join("daemon.res"));
    if spawn_self_detached(env, &["internal", "daemon"]).is_err() {
        log_err(env, "daemon", "守护进程未能启动");
        return false;
    }
    for _ in 0..10 {
        if run.join("daemon.ready").is_file() && daemon_running(env) {
            cgroup_escape_tree(env, std::process::id());
            let _ = ensure_watchdog(env);
            return true;
        }
        thread::sleep(Duration::from_secs(1));
    }
    log_err(env, "daemon", "守护进程未能启动");
    false
}

pub fn daemon_send(env: &Env, cmd: &str) -> i32 {
    if env.daemon_foreground {
        return 0;
    }
    if !ensure_daemon(env) {
        return 1;
    }
    let run = env.run_dir();
    let _ = fs::remove_file(run.join("daemon.res"));
    let _ = fs::write(run.join("daemon.cmd"), format!("{cmd}\n"));
    let p = daemon_pid(env);
    if !send_signal(&p, Signal::USR1) {
        return 1;
    }
    for _ in 0..30 {
        if run.join("daemon.res").is_file() {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    fs::read_to_string(run.join("daemon.res"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(1)
}

pub fn vm_start(env: &Env) -> i32 {
    if env.in_daemon || env.daemon_foreground {
        return vm_start_exec(env);
    }
    daemon_send(env, "start")
}

pub fn vm_stop(env: &Env) -> i32 {
    if env.in_daemon || env.daemon_foreground {
        return vm_stop_exec(env, false);
    }
    if daemon_running(env) {
        return daemon_send(env, "stop");
    }
    vm_stop_exec(env, false)
}

fn clean_daemon_pid(env: &Env, self_pid: &str) {
    let run = env.run_dir();
    if daemon_pid(env) == self_pid {
        let _ = fs::remove_file(run.join("daemon.pid"));
        let _ = fs::remove_file(run.join("daemon.ready"));
    }
}

pub fn daemon_loop(env: &Env) -> i32 {
    let _ = env.ensure_dirs();
    let run = env.run_dir();
    let self_pid = std::process::id().to_string();
    let _ = fs::write(run.join("daemon.pid"), format!("{self_pid}\n"));
    let _ = cgroup_escape(env, &self_pid);

    let hup = Arc::new(AtomicBool::new(false));
    let usr = Arc::new(AtomicBool::new(false));
    let term = Arc::new(AtomicBool::new(false));
    let _ = flag::register(SIGHUP, Arc::clone(&hup));
    let _ = flag::register(SIGUSR1, Arc::clone(&usr));
    let _ = flag::register(SIGUSR2, Arc::clone(&usr));
    let _ = flag::register(SIGTERM, Arc::clone(&term));
    let _ = flag::register(SIGINT, Arc::clone(&term));

    let _ = fs::write(run.join("daemon.cmd"), "");
    for f in [
        "notify.on",
        "notify.idle",
        "notify.last",
        "notify.ts",
        "notify.warned",
    ] {
        let _ = fs::remove_file(run.join(f));
    }
    let _ = fs::write(run.join("daemon.ready"), "ready\n");
    let cg = cgroup_path(env, &self_pid).unwrap_or_default();
    log_info(env, "daemon", &format!("已启动 pid={self_pid} cgroup={cg}"));

    let mut tick = 0_u64;
    let mut reset_times: Vec<u64> = Vec::new();
    loop {
        if term.load(Ordering::Relaxed) {
            if sys_shutting_down(env) {
                shutdown_note(env, "daemon");
            } else {
                log_info(env, "daemon", "收到 TERM，守护进程退出");
            }
            if !xr_vm_alive(env) {
                let _ = fs::remove_file(run.join("state"));
                let _ = fs::remove_file(run.join("ready_at"));
                let _ = fs::remove_file(run.join("crosvm.pid"));
                let _ = fs::remove_file(run.join("stopping"));
                routes_emergency_down(env);
            }
            clean_daemon_pid(env, &self_pid);
            return 0;
        }

        if usr.swap(false, Ordering::SeqCst) {
            let cmd = fs::read_to_string(run.join("daemon.cmd"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let _ = fs::write(run.join("daemon.cmd"), "");
            if !cmd.is_empty() {
                let rc = match cmd.as_str() {
                    "start" => {
                        reset_times.clear();
                        vm_start_exec(env)
                    }
                    "stop" => {
                        reset_times.clear();
                        vm_stop_exec(env, false)
                    }
                    "ping" => 0,
                    "quit" => {
                        let _ = vm_stop_exec(env, false);
                        let cfg = Config::load(env);
                        let ctx = NetCtx::new(env, &cfg);
                        acct_snapshot(&ctx);
                        notify_cancel(env);
                        clean_daemon_pid(env, &self_pid);
                        let _ = fs::remove_file(run.join("daemon.cmd"));
                        let _ = fs::write(run.join("daemon.res"), "0\n");
                        return 0;
                    }
                    _ => 2,
                };
                let _ = fs::write(run.join("daemon.res"), format!("{rc}\n"));
                if term.load(Ordering::Relaxed) && !sys_shutting_down(env) {
                    log_info(env, "daemon", "停止流程完成，响应 TERM 退出");
                    clean_daemon_pid(env, &self_pid);
                    return 0;
                }
                let _ = fs::write(run.join("notify.force"), "");
                notify_update(env);
            }
        }

        if sys_shutting_down(env) {
            shutdown_note(env, "daemon");
            clean_daemon_pid(env, &self_pid);
            return 0;
        }

        cgroup_escape_tree(env, std::process::id());
        if !vm_stopping(env) && (run.join("crosvm.pid").is_file() || run.join("state").is_file()) {
            if !vm_running(env) {
                if !handle_guest_reboot(env, &mut reset_times) {
                    let reason = vm_exit_reason(env);
                    emergency_teardown(env, &reason);
                }
            } else if fs::read_to_string(run.join("state")).is_ok_and(|s| s.trim() == "ready") {
                let fw_pid = fs::read_to_string(run.join("fuse_watch.pid"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !is_pid_running(env, &fw_pid) {
                    let cfg = Config::load(env);
                    let cp = fs::read_to_string(run.join("crosvm.pid"))
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let since = fs::read_to_string(run.join("ready_at"))
                        .ok()
                        .and_then(|s| s.trim().parse::<u64>().ok())
                        .or_else(|| {
                            fs::read_to_string(env.proc_base.join(&cp).join("stat"))
                                .ok()
                                .and_then(|s| {
                                    s.split_whitespace()
                                        .nth(21)
                                        .and_then(|x| x.parse::<u64>().ok())
                                        .map(|ticks| ticks / 100)
                                })
                        });
                    if let (Some(r0), Some(now)) = (since, crate::vm::mono_now(env)) {
                        if now.saturating_sub(r0) >= cfg.stable_sec() {
                            let _ = fs::remove_file(env.fuse_file());
                            let _ = fs::remove_file(env.boot_log());
                            let _ = fs::write(
                                env.last_good_file(),
                                format!("{}\n", crate::config::config_hash(env)),
                            );
                            let _ = fs::write(run.join("state"), "stable\n");
                            log_info(env, "vm", "稳定，熔断已解除，last_good 已更新");
                        }
                    }
                }
            }
        }

        if stale_gateway(env) {
            let reason = vm_exit_reason(env);
            emergency_teardown(
                env,
                &format!("{reason}（xr0 无载波或虚拟机已死，规则仍在）"),
            );
        }
        notify_update(env);
        rotate_all_logs(env);

        tick = tick.saturating_add(1);
        if tick % 12 == 0 {
            let cfg = Config::load(env);
            let ctx = NetCtx::new(env, &cfg);
            acct_snapshot(&ctx);
            debug_expire(env);
        }

        for _ in 0..5 {
            if usr.load(Ordering::Relaxed) || term.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_secs(1));
            if run.join("notify.force").is_file() {
                notify_update(env);
            }
        }
    }
}
