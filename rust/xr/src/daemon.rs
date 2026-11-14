//! 常驻守护进程：串行执行 `start`/`stop` 等命令、发现客机自行重启后重新拉起，并由看门狗互相保活。

/// 守护进程的保活工具：cgroup 迁移、常驻通知与异常退出清理。
pub mod keepalive;
/// 看门狗进程：定期检查守护进程与网关残留。
pub(crate) mod stats;
pub(crate) mod watchdog;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
use crate::log;
use crate::log::note_ok;
use crate::log::note_rc;
use crate::log::{debug_expire, rotate_all_logs, unix_now};
use crate::net::common::NetCtx;
use crate::sys;
use crate::sys::fsx;
use crate::sys::send_signal_logged;
use crate::sys::spawn;
use crate::sys::{is_pid_running, proc_cmdline, send_signal, shutdown_note};
use crate::traffic::acct_snapshot;
use crate::vm;
use crate::vm::run_net_hook;

/// 客机自行重启导致 `crosvm` 退出时清理并重新启动虚拟机，已处理返回 true。
///
/// 2 分钟内已连续 3 次时记 ERROR 并不再自动拉起。
fn handle_guest_reboot(env: &Env, reset_times: &mut Vec<u64>) -> bool {
    if !vm_guest_reset_pending(env) {
        return false;
    }
    let now = unix_now();
    reset_times.retain(|&ts| now.saturating_sub(ts) < 120);
    if reset_times.len() >= 3 {
        log::err(env, "vm", "短时间内连续多次检测到客机重启，停止自动拉起");
        return false;
    }
    reset_times.push(now);

    let run = env.run_dir();
    log::info(env, "vm", "检测到 OpenWrt 重启请求，正在重新拉起服务");
    fsx::write_logged(
        env,
        run.join("stopping"),
        format!("{}\n", std::process::id()),
    );
    if let Ok(wp) = fs::read_to_string(run.join("fuse_watch.pid")) {
        let wp_trimmed = wp.trim();
        if !wp_trimmed.is_empty() && is_pid_running(env, wp_trimmed) {
            send_signal_logged(env, wp_trimmed, Signal::TERM);
        }
    }
    crate::clash::stop_clash_mux(env);
    for path in [
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
        fsx::remove_logged(env, path);
    }
    note_rc(env, "run_net_hook", run_net_hook(env, "down"));
    fsx::remove_logged(env, run.join("stopping"));

    note_rc(env, "vm_start_exec", vm::start_exec(env));
    fsx::write_logged(env, run.join("notify.force"), "");
    notify_update(env);
    true
}

/// `run/daemon.pid` 中记录的守护进程号（去掉空白），没有时为空串。
#[must_use]
pub(crate) fn pid(env: &Env) -> String {
    fs::read_to_string(env.run_dir().join("daemon.pid"))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// 守护进程在运行时返回 true（能读到命令行时还要求含 `daemon` 或 `xr`）。
#[must_use]
pub(crate) fn running(env: &Env) -> bool {
    let pid = pid(env);
    if !is_pid_running(env, &pid) {
        return false;
    }
    if let Some(cmd) = proc_cmdline(env, &pid) {
        return cmd.contains("daemon") || cmd.contains("xr");
    }
    true
}

/// 确保看门狗在运行（守护进程与前台模式下无需看门狗）；需要时后台启动并最多等 8 秒，失败记 ERROR 返回 false。
#[must_use]
pub(crate) fn ensure_watchdog(env: &Env) -> bool {
    if env.role.is_watchdog() || env.daemon_mode.is_foreground() {
        return true;
    }
    let run = env.run_dir();
    let wp = fs::read_to_string(run.join("watchdog.pid"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if is_pid_running(env, &wp)
        && let Some(cmd) = proc_cmdline(env, &wp)
        && (cmd.contains("watchdog") || cmd.contains("xr"))
    {
        note_ok(env, "cgroup_escape", cgroup_escape(env, &wp));
        return true;
    }
    if spawn::self_detached(env, &["internal", "watchdog"]).is_err() {
        log::err(env, "daemon", "看门狗未能启动");
        return false;
    }
    for _ in 0_i32..8_i32 {
        let pid_text = fs::read_to_string(run.join("watchdog.pid"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        if is_pid_running(env, &pid_text) {
            note_ok(env, "cgroup_escape", cgroup_escape(env, &pid_text));
            return true;
        }
        thread::sleep(Duration::from_secs(1));
    }
    log::err(env, "daemon", "看门狗未能启动");
    false
}

/// 确保守护进程在运行；需要时后台启动并最多等 10 秒就绪，同时迁移 cgroup 并拉起看门狗，失败记 ERROR 返回 false。
#[must_use]
pub(crate) fn ensure(env: &Env) -> bool {
    if env.daemon_mode.is_foreground() {
        return true;
    }
    if let Err(err) = env.ensure_dirs() {
        fsx::record_failure(env, "ensure_dirs", &env.xr_data, &err);
    }
    if running(env) {
        cgroup_escape_tree(env, std::process::id());
        note_ok(env, "ensure_watchdog", ensure_watchdog(env));
        return true;
    }
    let run = env.run_dir();
    fsx::remove_logged(env, run.join("daemon.ready"));
    fsx::remove_logged(env, run.join("daemon.res"));
    if spawn::self_detached(env, &["internal", "daemon"]).is_err() {
        log::err(env, "daemon", "守护进程未能启动");
        return false;
    }
    for _ in 0_i32..10_i32 {
        if run.join("daemon.ready").is_file() && running(env) {
            cgroup_escape_tree(env, std::process::id());
            note_ok(env, "ensure_watchdog", ensure_watchdog(env));
            return true;
        }
        thread::sleep(Duration::from_secs(1));
    }
    log::err(env, "daemon", "守护进程未能启动");
    false
}

/// 把命令写入 `run/daemon.cmd`，用 `SIGUSR1` 通知守护进程，最多等 30 秒读取 `run/daemon.res` 作为退出码（取不到为 1）。
///
/// 前台模式下直接返回 0。
#[must_use]
pub(crate) fn send(env: &Env, cmd: &str) -> i32 {
    if env.daemon_mode.is_foreground() {
        return 0;
    }
    if !ensure(env) {
        return 1;
    }
    let run = env.run_dir();
    fsx::remove_logged(env, run.join("daemon.res"));
    fsx::write_logged(env, run.join("daemon.cmd"), format!("{cmd}\n"));
    let pid = pid(env);
    if !send_signal(&pid, Signal::USR1) {
        return 1;
    }
    for _ in 0_i32..30_i32 {
        if run.join("daemon.res").is_file() {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    fs::read_to_string(run.join("daemon.res"))
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .unwrap_or(1)
}

/// `xr start`：在守护进程内或前台模式下直接启动，否则交给守护进程执行。
#[must_use]
pub(crate) fn vm_start(env: &Env) -> i32 {
    if env.role.is_daemon() || env.daemon_mode.is_foreground() {
        return vm::start_exec(env);
    }
    send(env, "start")
}

/// `xr stop`：在守护进程内或前台模式下直接停止；守护进程在运行时交给它执行，否则直接停止。
#[must_use]
pub(crate) fn vm_stop(env: &Env) -> i32 {
    if env.role.is_daemon() || env.daemon_mode.is_foreground() {
        return vm::stop_exec(env, false);
    }
    if running(env) {
        return send(env, "stop");
    }
    vm::stop_exec(env, false)
}

/// 守护进程号仍是自己时删除 `daemon.pid` 与 `daemon.ready`。
fn clean_daemon_pid(env: &Env, self_pid: &str) {
    let run = env.run_dir();
    if pid(env) == self_pid {
        fsx::remove_logged(env, run.join("daemon.pid"));
        fsx::remove_logged(env, run.join("daemon.ready"));
    }
}

/// 守护进程主循环的运行状态。
#[derive(Debug)]
struct DaemonState {
    /// 已走过的循环拍数。
    tick: u64,
    /// 近期客机自行重启的时刻（用于限频）。
    reset_times: Vec<u64>,
    /// 收到 `SIGUSR1`/`SIGUSR2`。
    usr: Arc<AtomicBool>,
    /// 收到 `SIGTERM`/`SIGINT`。
    term: Arc<AtomicBool>,
    /// 本进程号。
    self_pid: String,
}

/// 把信号 `sig`（名字 `name`）登记为置位 `flag_val`；失败只记日志。
fn register_flag(env: &Env, sig: i32, name: &str, flag_val: &Arc<AtomicBool>) {
    if let Err(err) = flag::register(sig, Arc::clone(flag_val)) {
        log::err(env, "daemon", &format!("信号注册失败 {name}: {err}"));
    }
}

/// 守护进程启动：写 `daemon.pid`、逃出应用 `cgroup`、登记信号、清空命令与通知状态、写 `daemon.ready`。
fn daemon_init(env: &Env) -> DaemonState {
    if let Err(err) = env.ensure_dirs() {
        fsx::record_failure(env, "ensure_dirs", &env.xr_data, &err);
    }
    let run = env.run_dir();
    let self_pid = std::process::id().to_string();
    fsx::write_logged(env, run.join("daemon.pid"), format!("{self_pid}\n"));
    note_ok(env, "cgroup_escape", cgroup_escape(env, &self_pid));

    let hup = Arc::new(AtomicBool::new(false));
    let usr = Arc::new(AtomicBool::new(false));
    let term = Arc::new(AtomicBool::new(false));
    register_flag(env, SIGHUP, "SIGHUP", &hup);
    register_flag(env, SIGUSR1, "SIGUSR1", &usr);
    register_flag(env, SIGUSR2, "SIGUSR2", &usr);
    register_flag(env, SIGTERM, "SIGTERM", &term);
    register_flag(env, SIGINT, "SIGINT", &term);

    fsx::write_logged(env, run.join("daemon.cmd"), "");
    for name in [
        "notify.on",
        "notify.idle",
        "notify.last",
        "notify.ts",
        "notify.warned",
    ] {
        fsx::remove_logged(env, run.join(name));
    }
    fsx::write_logged(env, run.join("daemon.ready"), "ready\n");
    let cg = cgroup_path(env, &self_pid).unwrap_or_default();
    log::info(env, "daemon", &format!("已启动 pid={self_pid} cgroup={cg}"));
    DaemonState {
        tick: 0,
        reset_times: Vec::new(),
        usr,
        term,
        self_pid,
    }
}

/// 收到 `TERM` 时：记日志，虚拟机已不在时清理运行状态与路由，删除自己的 `daemon.pid`；返回 `Some(0)` 表示退出。
fn on_term(env: &Env, st: &DaemonState) -> Option<i32> {
    if !st.term.load(Ordering::Relaxed) {
        return None;
    }
    if sys::shutting_down(env) {
        shutdown_note(env, "daemon");
    } else {
        log::info(env, "daemon", "收到 TERM，守护进程退出");
    }
    if !xr_vm_alive(env) {
        let run = env.run_dir();
        fsx::remove_logged(env, run.join("state"));
        fsx::remove_logged(env, run.join("ready_at"));
        fsx::remove_logged(env, run.join("crosvm.pid"));
        fsx::remove_logged(env, run.join("stopping"));
        routes_emergency_down(env);
    }
    clean_daemon_pid(env, &st.self_pid);
    Some(0)
}

/// `quit` 命令：停虚拟机、保存流量快照、撤销通知、删除 `daemon.pid` 与命令文件并写结果 0。
fn quit(env: &Env, st: &DaemonState) {
    let run = env.run_dir();
    note_rc(env, "vm_stop_exec", vm::stop_exec(env, false));
    let cfg = Config::load(env);
    let ctx = NetCtx::new(env, &cfg);
    acct_snapshot(&ctx);
    notify_cancel(env);
    clean_daemon_pid(env, &st.self_pid);
    fsx::remove_logged(env, run.join("daemon.cmd"));
    fsx::write_logged(env, run.join("daemon.res"), "0\n");
}

/// 处理 `SIGUSR1` 送来的 `daemon.cmd`（`start`、`stop`、`ping`、`quit`），写 `daemon.res` 并刷新通知；返回 `Some(0)` 表示退出。
fn on_command(env: &Env, st: &mut DaemonState) -> Option<i32> {
    if !st.usr.swap(false, Ordering::SeqCst) {
        return None;
    }
    let run = env.run_dir();
    let cmd = fs::read_to_string(run.join("daemon.cmd"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    fsx::write_logged(env, run.join("daemon.cmd"), "");
    if cmd.is_empty() {
        return None;
    }
    let rc = match cmd.as_str() {
        "start" => {
            st.reset_times.clear();
            vm::start_exec(env)
        }
        "stop" => {
            st.reset_times.clear();
            vm::stop_exec(env, false)
        }
        "ping" => 0_i32,
        "quit" => {
            quit(env, st);
            return Some(0);
        }
        _ => 2_i32,
    };
    fsx::write_logged(env, run.join("daemon.res"), format!("{rc}\n"));
    if st.term.load(Ordering::Relaxed) && !sys::shutting_down(env) {
        log::info(env, "daemon", "停止流程完成，响应 TERM 退出");
        clean_daemon_pid(env, &st.self_pid);
        return Some(0);
    }
    fsx::write_logged(env, run.join("notify.force"), "");
    notify_update(env);
    None
}

/// 虚拟机就绪的起点（秒）：优先 `ready_at`，否则 `crosvm`（进程号 `cp`）的启动时间。
fn ready_since(env: &Env, cp: &str) -> Option<u64> {
    fs::read_to_string(env.run_dir().join("ready_at"))
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .or_else(|| {
            let text = fs::read_to_string(env.proc_base.join(cp).join("stat")).ok()?;
            text.split_whitespace()
                .nth(21)
                .and_then(|x| x.parse::<u64>().ok())
                .map(|ticks| ticks.checked_div(100_u64).unwrap_or(0_u64))
        })
}

/// 状态停在 `ready` 而保险丝看守进程已不在时，就绪满 `STABLE_SEC` 后代为拆除保险丝、写 `last_good` 并记为 `stable`。
fn promote_stable(env: &Env) {
    let run = env.run_dir();
    let fw_pid = fs::read_to_string(run.join("fuse_watch.pid"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if is_pid_running(env, &fw_pid) {
        return;
    }
    let cfg = Config::load(env);
    let cp = fs::read_to_string(run.join("crosvm.pid"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if let (Some(r0), Some(now)) = (ready_since(env, &cp), vm::mono_now(env))
        && now.saturating_sub(r0) >= cfg.stable_sec()
    {
        fsx::remove_logged(env, env.fuse_file());
        fsx::remove_logged(env, env.boot_log());
        fsx::write_logged(
            env,
            env.last_good_file(),
            format!("{}\n", crate::config::hash(env)),
        );
        fsx::write_logged(env, run.join("state"), "stable\n");
        log::info(env, "vm", "稳定，熔断已解除，last_good 已更新");
    }
}

/// 每拍检查虚拟机：本应运行却已退出时先看是否客机自行重启，否则紧急拆除；`ready` 状态下代为推进到 `stable`。
fn watch_vm(env: &Env, st: &mut DaemonState) {
    let run = env.run_dir();
    if vm_stopping(env) || !(run.join("crosvm.pid").is_file() || run.join("state").is_file()) {
        return;
    }
    if !vm::running(env) {
        if !handle_guest_reboot(env, &mut st.reset_times) {
            let reason = vm_exit_reason(env);
            emergency_teardown(env, &reason);
        }
    } else if fs::read_to_string(run.join("state")).is_ok_and(|content| content.trim() == "ready") {
        promote_stable(env);
    } else {
        // 虚拟机在运行但尚未 ready：本轮不检查 fuse 看门进程
    }
}

/// 每拍的例行工作：网关残留检查、更新通知、轮转日志；每 12 拍保存流量快照并检查调试模式到期。
fn periodic(env: &Env, st: &mut DaemonState) {
    if stale_gateway(env) {
        let reason = vm_exit_reason(env);
        emergency_teardown(
            env,
            &format!("{reason}（xr0 无载波或虚拟机已死，规则仍在）"),
        );
    }
    notify_update(env);
    rotate_all_logs(env);

    st.tick = st.tick.saturating_add(1);
    if st.tick.is_multiple_of(12) {
        let cfg = Config::load(env);
        let ctx = NetCtx::new(env, &cfg);
        acct_snapshot(&ctx);
        debug_expire(env);
    }
    if st.tick.is_multiple_of(TZ_REFRESH_TICKS) {
        refresh_tz(env);
    }
}

/// 时区重新探测的间隔拍数：每拍约 5 秒，720 拍约 1 小时。
const TZ_REFRESH_TICKS: u64 = 720;

/// 重新探测手机时区（跟上用户改时区或夏令时）；偏移有变化时写一行调试日志。
fn refresh_tz(env: &Env) {
    let before = log::tz_cached();
    let after = log::tz_refresh();
    if before != Some(after) {
        log::dlog_env(env, "daemon", &format!("时区偏移更新为 {after}"));
    }
}

/// 拍间休眠：最多 5 秒，每秒醒来一次；收到命令或 `TERM` 立即结束，有 `notify.force` 时刷新通知。
fn nap(env: &Env, st: &DaemonState) {
    let run = env.run_dir();
    for _ in 0_i32..5_i32 {
        if st.usr.load(Ordering::Relaxed) || st.term.load(Ordering::Relaxed) {
            break;
        }
        thread::sleep(Duration::from_secs(1));
        if run.join("notify.force").is_file() {
            notify_update(env);
        }
    }
}

/// 守护进程主循环：响应 `SIGUSR1` 命令（`start`、`stop`、`ping`、`quit`）、`SIGUSR2` 客机重启与 `SIGTERM` 退出，并周期更新常驻通知；返回进程退出码。
#[must_use]
pub(crate) fn main_loop(env: &Env) -> i32 {
    let mut st = daemon_init(env);
    let server = stats::start(env);
    let code = run_loop(env, &mut st);
    stats::shutdown(env, server);
    code
}

/// 主循环本体：处理 `TERM`、命令与关机，否则看护虚拟机、做周期任务并休眠；返回退出码。
fn run_loop(env: &Env, st: &mut DaemonState) -> i32 {
    loop {
        if let Some(code) = on_term(env, st) {
            return code;
        }
        if let Some(code) = on_command(env, st) {
            return code;
        }
        if sys::shutting_down(env) {
            shutdown_note(env, "daemon");
            clean_daemon_pid(env, &st.self_pid);
            return 0;
        }
        cgroup_escape_tree(env, std::process::id());
        watch_vm(env, st);
        periodic(env, st);
        nap(env, st);
    }
}
