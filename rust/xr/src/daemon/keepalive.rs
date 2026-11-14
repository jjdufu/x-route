//! 守护进程的保活工具：把相关进程移出应用 cgroup、常驻通知的启动与更新、`crosvm` 异常退出的识别与紧急清理。

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use rustix::process::Signal;

use crate::config::Config;
use crate::diag;
use crate::env::Env;
use crate::log;
use crate::log::note_ok;
use crate::log::note_rc;
use crate::log::unix_now;
use crate::net::common::NetCtx;
use crate::sys;
use crate::sys::exec::note_status;
use crate::sys::fsx;
use crate::sys::send_signal_logged;
use crate::sys::{is_pid_running, proc_cmdline, shutdown_note};
use crate::traffic::acct_total;
use crate::vm;
use crate::vm::run_net_hook;

#[cfg(test)]
mod tests;

/// 把进程移到系统 cgroup（v2 根或 `system`，失败时 v1 `uid_0`），写入各后台 cpuset/stune 组，并把 OOM 分数设为 -1000。
///
/// 进程不在运行时返回 false。
#[must_use]
pub(crate) fn cgroup_escape(env: &Env, pid: &str) -> bool {
    let trimmed = pid.trim();
    if trimmed.is_empty() || !is_pid_running(env, trimmed) {
        return false;
    }
    let root = &env.cgroup_root;
    let mut v2_ok = false;
    for procs_file in [root.join("cgroup.procs"), root.join("system/cgroup.procs")] {
        if fs::write(&procs_file, format!("{trimmed}\n")).is_ok() {
            v2_ok = true;
        }
    }
    if !v2_ok {
        let uid0 = root.join("uid_0/cgroup.procs");
        fsx::write_logged(env, &uid0, format!("{trimmed}\n"));
    }
    for name in [
        "/dev/memcg/system/cgroup.procs",
        "/dev/cpuset/system-background/cgroup.procs",
        "/dev/cpuset/system-background/tasks",
        "/dev/cpuset/background/cgroup.procs",
        "/dev/cpuset/background/tasks",
        "/dev/stune/background/cgroup.procs",
        "/dev/stune/background/tasks",
    ] {
        let path = Path::new(name);
        if path.exists() {
            fsx::write_logged(env, path, format!("{trimmed}\n"));
        }
    }
    let oom = env.proc_base.join(trimmed).join("oom_score_adj");
    fsx::write_logged(env, oom, "-1000\n");
    true
}

/// 对本进程和 `run/` 下记录的各后台进程（`crosvm`、对账循环、看守、看门狗、守护进程、通知器）执行 [`cgroup_escape`]，失败只写调试日志。
pub(crate) fn cgroup_escape_tree(env: &Env, self_pid: u32) {
    note_ok(
        env,
        "cgroup_escape",
        cgroup_escape(env, &self_pid.to_string()),
    );
    let run = env.run_dir();
    for name in [
        "crosvm.pid",
        "net_watch.pid",
        "fuse_watch.pid",
        "watchdog.pid",
        "daemon.pid",
        "notifier.pid",
    ] {
        if let Ok(pid_text) = fs::read_to_string(run.join(name)) {
            let trimmed = pid_text.trim();
            if !trimmed.is_empty() {
                note_ok(env, "cgroup_escape", cgroup_escape(env, trimmed));
            }
        }
    }
}

/// 读取进程所在 cgroup 路径：优先 cgroup v2 的 `0::` 行，否则取 v1 行的路径段。
#[must_use]
pub(crate) fn cgroup_path(env: &Env, pid: &str) -> Option<String> {
    let content = fs::read_to_string(env.proc_base.join(pid.trim()).join("cgroup")).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("0::") {
            return Some(rest.to_owned());
        }
        // cgroup v1 行格式 `层级号:控制器:路径`，取第三段
        if let Some(rest) = line.splitn(3, ':').nth(2) {
            return Some(rest.to_owned());
        }
    }
    None
}

/// `crosvm` 在运行时返回 true；读不到命令行时还会用 `proc_base` 下的进程目录与存活检查兜底确认。
#[must_use]
pub(crate) fn xr_vm_alive(env: &Env) -> bool {
    if vm::running(env) {
        return true;
    }
    let pid = fs::read_to_string(env.run_dir().join("crosvm.pid")).unwrap_or_default();
    let trimmed = pid.trim();
    if trimmed.is_empty() || !env.proc_base.join(trimmed).is_dir() {
        return false;
    }
    if !is_pid_running(env, trimmed) {
        return false;
    }
    proc_cmdline(env, trimmed).is_some_and(|cmdline| cmdline.contains("crosvm"))
}

/// 正在执行停止（`run/stopping` 中记录的进程仍在运行）时返回 true。
#[must_use]
pub(crate) fn vm_stopping(env: &Env) -> bool {
    let st = env.run_dir().join("stopping");
    if !st.is_file() {
        return false;
    }
    let sp = fs::read_to_string(st).unwrap_or_default();
    is_pid_running(env, sp.trim())
}

/// 常驻通知显示的状态文字：`stable` 为"运行中"、`ready` 为"已就绪"，其余为"启动中"。
#[must_use]
pub(crate) fn notify_state(env: &Env) -> &'static str {
    let text = fs::read_to_string(env.run_dir().join("state")).unwrap_or_default();
    match text.trim() {
        "stable" => "运行中",
        "ready" => "已就绪",
        _ => "启动中",
    }
}

/// 发给通知器的一行内容：`状态 口径 出网上行 出网下行 局域网上行 局域网下行`。
#[must_use]
pub(crate) fn notify_payload(env: &Env, cfg: &Config) -> String {
    let mode = cfg.notify_traffic();
    let ctx = NetCtx::new(env, cfg);
    let [wu, wd, lu, ld] = acct_total(&ctx);
    format!("{} {mode} {wu} {wd} {lu} {ld}", notify_state(env))
}

/// 屏幕是否点亮：`dumpsys power` 的 `mWakefulness` 为 `Asleep` 或 `Dozing` 时为灭屏；测试可用 `XR_SCREEN` 覆盖。
pub(crate) fn screen_on(env: &Env) -> bool {
    if let Some(raw) = &env.screen_override {
        return raw == "on";
    }
    let Ok(out) = Command::new("dumpsys").arg("power").output() else {
        return true;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let Some(pos) = line.find("mWakefulness=") else {
            continue;
        };
        let rest = line
            .get(pos.saturating_add("mWakefulness=".len())..)
            .unwrap_or("");
        let word: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
        if matches!(word.as_str(), "Asleep" | "Dozing") {
            return false;
        }
    }
    true
}

/// 通知器进程在运行（`run/notifier.pid` 存在且命令行含 `xrnotify`）时返回 true。
#[must_use]
pub(crate) fn notifier_running(env: &Env) -> bool {
    let pid = fs::read_to_string(env.run_dir().join("notifier.pid")).unwrap_or_default();
    let trimmed = pid.trim();
    if trimmed.is_empty() || !is_pid_running(env, trimmed) {
        return false;
    }
    proc_cmdline(env, trimmed).is_some_and(|cmdline| cmdline.contains("xrnotify"))
}

/// 用 `app_process` 同步运行一次通知器（`com.xrouter.notify.Main <mode> <run目录>`），失败只计数。
fn notifier_exec(env: &Env, mode: &str) {
    let cp = format!("-Djava.class.path={}", env.notifier_jar.display());
    let status = Command::new(&env.app_process)
        .args([
            &cp,
            "/system/bin",
            "--nice-name=xrnotify",
            "com.xrouter.notify.Main",
            mode,
            &env.run_dir().to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    note_status(env, "xrnotify", status);
}

/// 通知器未运行时在后台启动它（清掉旧的请求与回执，输出写 `run/notifier.log`），已在运行或启动成功返回 true。
pub(crate) fn notifier_start(env: &Env) -> bool {
    if notifier_running(env) {
        return true;
    }
    if !env.notifier_jar.is_file() {
        return false;
    }
    let run = env.run_dir();
    fsx::remove_logged(env, run.join("notify.req"));
    fsx::remove_logged(env, run.join("notify.ack"));
    let nlog = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(run.join("notifier.log"));
    let stderr_cfg = nlog.map_or_else(|_| Stdio::null(), Stdio::from);
    let cp = format!("-Djava.class.path={}", env.notifier_jar.display());
    let child = Command::new(&env.app_process)
        .args([
            &cp,
            "/system/bin",
            "--nice-name=xrnotify",
            "com.xrouter.notify.Main",
            "serve",
            &run.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_cfg)
        .spawn();
    let Ok(ch) = child else {
        return false;
    };
    let pid_s = ch.id().to_string();
    fsx::write_logged(env, run.join("notifier.pid"), format!("{pid_s}\n"));
    note_ok(env, "cgroup_escape", cgroup_escape(env, &pid_s));
    true
}

/// 原子写入一条通知请求 `run/notify.req`，成功返回 true。
fn notifier_req(env: &Env, payload: &str) -> bool {
    let run = env.run_dir();
    let tmp = run.join("notify.req.tmp");
    let dst = run.join("notify.req");
    fs::write(&tmp, format!("{payload}\n")).is_ok() && fs::rename(&tmp, &dst).is_ok()
}

/// 在 `XR_NOTIFY_WAIT` 次（每次 200 毫秒）内等待回执等于 `expected`；回执以 `err ` 开头或超时返回 false。
fn notifier_wait(env: &Env, expected: &str) -> bool {
    let ack = env.run_dir().join("notify.ack");
    for _ in 0..env.notify_wait {
        if let Ok(ack_text) = fs::read_to_string(&ack) {
            let trimmed = ack_text.trim();
            if trimmed == expected {
                return true;
            }
            if trimmed.starts_with("err ") {
                return false;
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
    false
}

/// 启动通知器（必要时）并发送一条通知内容，收到对应 `ok` 回执时返回 true。
#[must_use]
pub(crate) fn notifier_post(env: &Env, payload: &str) -> bool {
    if !notifier_start(env) || !notifier_req(env, payload) {
        return false;
    }
    notifier_wait(env, &format!("ok {payload}"))
}

/// 撤销常驻通知：通知器在运行时发 `cancel` 并结束它，未确认撤销时再同步执行一次 `cancel`。
pub(crate) fn notify_cancel(env: &Env) {
    let run = env.run_dir();
    let mut done = false;
    if notifier_running(env) {
        if notifier_req(env, "cancel") && notifier_wait(env, "ok cancel") {
            done = true;
        }
        if let Ok(pid_text) = fs::read_to_string(run.join("notifier.pid")) {
            send_signal_logged(env, pid_text.trim(), Signal::TERM);
        }
    }
    if !done && env.notifier_jar.is_file() {
        notifier_exec(env, "cancel");
    }
    for name in [
        "notifier.pid",
        "notify.req",
        "notify.ack",
        "notify.on",
        "notify.last",
        "notify.ts",
    ] {
        fsx::remove_logged(env, run.join(name));
    }
}

/// 常驻通知发送失败的原因：优先用 `notify.ack` 回执，没有时取 `notifier.log` 最后 3 行，仍为空则为"无回执"。
fn notify_fail_reason(env: &Env) -> String {
    let run = env.run_dir();
    let ack = fs::read_to_string(run.join("notify.ack")).unwrap_or_default();
    if !ack.trim().is_empty() {
        return ack.trim().to_owned();
    }
    let nlog = fs::read_to_string(run.join("notifier.log")).unwrap_or_default();
    let lines: Vec<&str> = nlog.lines().collect();
    let start = lines.len().saturating_sub(3);
    let joined = lines.get(start..).unwrap_or(&[]).join(" ");
    if joined.trim().is_empty() {
        "无回执".to_owned()
    } else {
        joined
    }
}

/// 通知节流状态（仅守护进程内存；守护进程重启即重置，首轮会立即刷新一次通知，与旧版启动时删除 `notify.ts` 的效果一致）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct NotifyMem {
    /// 上次检查时刻（unix 秒）；0 表示尚未检查。
    pub(crate) ts: u64,
    /// 上次成功投递的负载（已去掉首尾空白）。
    pub(crate) last: String,
}

/// `steady`（通知已在显示且未强制刷新）时，距上次检查不足 `interval` 秒则本轮跳过。
#[must_use]
pub(crate) const fn notify_throttled(
    steady: bool,
    now: u64,
    mem: &NotifyMem,
    interval: u64,
) -> bool {
    steady && now.saturating_sub(mem.ts) < interval
}

/// `steady`（通知已在显示且未强制刷新）时，负载与上次相同或熄屏则不重发（只更新检查时刻）；`screen_on` 只在需要时才调用。
#[must_use]
pub(crate) fn notify_unchanged<F: FnOnce() -> bool>(
    steady: bool,
    mem: &NotifyMem,
    payload: &str,
    screen_on: F,
) -> bool {
    steady && (mem.last == payload || !screen_on())
}

/// 更新常驻通知：虚拟机不在时撤销；在运行时按间隔、内容是否变化和亮屏状态决定是否重发，首次发送失败记一次 ERROR。
///
/// 检查时刻与上次负载放在 `mem` 里，不写盘；`notify.on`、`notify.idle`、`notify.force`、`notify.warned` 等跨进程标志文件不变。
pub(crate) fn notify_update(env: &Env, mem: &mut NotifyMem) {
    let run = env.run_dir();
    if !xr_vm_alive(env) {
        if run.join("notify.on").is_file() || !run.join("notify.idle").is_file() {
            notify_cancel(env);
            *mem = NotifyMem::default();
            fsx::write_logged(env, run.join("notify.idle"), "");
        }
        return;
    }
    fsx::remove_logged(env, run.join("notify.idle"));
    let force = if run.join("notify.force").is_file() {
        fsx::remove_logged(env, run.join("notify.force"));
        true
    } else {
        false
    };
    if run.join("notify.on").is_file() && !notifier_running(env) {
        fsx::remove_logged(env, run.join("notify.on"));
    }
    let now = unix_now();
    if notify_throttled(
        run.join("notify.on").is_file() && !force,
        now,
        mem,
        env.notify_interval,
    ) {
        return;
    }
    let cfg = Config::load(env);
    let pl = notify_payload(env, &cfg);
    let shown = run.join("notify.on").is_file();
    if notify_unchanged(shown && !force, mem, &pl, || screen_on(env)) {
        mem.ts = now;
        return;
    }
    mem.ts = now;
    if notifier_post(env, &pl) {
        pl.trim().clone_into(&mut mem.last);
        fsx::write_logged(env, run.join("notify.on"), "");
        fsx::remove_logged(env, run.join("notify.warned"));
        return;
    }
    if !run.join("notify.warned").is_file() {
        let short: String = notify_fail_reason(env).chars().take(200).collect();
        log::err(env, "daemon", &format!("常驻通知发送失败：{short}"));
        fsx::write_logged(env, run.join("notify.warned"), "");
    }
}

/// `vmm.log` 中每次启动 `crosvm` 前写入的分隔行前缀。
pub const VMM_RUN_MARKER: &str = "=== xr crosvm start ";

/// `vmm.log` 中最近一次启动之后的部分（找不到分隔行时为全文）。
#[must_use]
pub fn vmm_last_run_slice(vmm: &str) -> &str {
    if let Some(idx) = vmm.rfind(VMM_RUN_MARKER) {
        let after = vmm.get(idx..).unwrap_or("");
        if let Some(nl) = after.find('\n') {
            return after.get(nl.saturating_add(1)..).unwrap_or("");
        }
        return "";
    }
    vmm
}

/// 最近一次运行的 `vmm.log` 含客机重启请求（`vcpu requested reset` 或 `system reset event`）时返回 true。
#[must_use]
pub fn vmm_has_guest_reset(vmm: &str) -> bool {
    let cur = vmm_last_run_slice(vmm);
    cur.contains("vcpu requested reset") || cur.contains("system reset event")
}

/// 客机自行重启导致 `crosvm` 退出、需要重新拉起时返回 true：不在停止或关机中、`crosvm` 已不在、上次状态为就绪且日志含重启请求。
#[must_use]
pub fn vm_guest_reset_pending(env: &Env) -> bool {
    if vm_stopping(env) || xr_vm_alive(env) || sys::shutting_down(env) {
        return false;
    }
    let st = fs::read_to_string(env.run_dir().join("state")).unwrap_or_default();
    if !matches!(st.trim(), "ready" | "stable") {
        return false;
    }
    fs::read_to_string(env.vmm_log()).is_ok_and(|text| vmm_has_guest_reset(&text))
}

/// `crosvm` 的退出原因：仍在运行时说明仍在运行，否则取最近一次运行的 `vmm.log` 最后 8 行。
#[must_use]
pub fn vm_exit_reason(env: &Env) -> String {
    let pid_text = fs::read_to_string(env.run_dir().join("crosvm.pid"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !pid_text.is_empty() && is_pid_running(env, &pid_text) {
        return "crosvm 仍在运行".to_owned();
    }
    if let Ok(vmm) = fs::read_to_string(env.vmm_log()) {
        let cur = vmm_last_run_slice(&vmm);
        let lines: Vec<&str> = cur.lines().collect();
        let start = lines.len().saturating_sub(8);
        let tail = lines
            .get(start..)
            .unwrap_or(&[])
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !tail.is_empty() {
            return format!("crosvm 已退出：{tail}");
        }
    }
    "crosvm 已退出，日志未记录退出原因".to_owned()
}

/// 紧急撤销手机侧网络（执行 `net down`），返回码非 0 时写调试日志。
pub(crate) fn routes_emergency_down(env: &Env) {
    note_rc(env, "run_net_hook", run_net_hook(env, "down"));
}

/// 关机中虚拟机已不在时只做必要清理：停分流、删运行状态、撤销网络、释放唤醒锁、删 `net.gw`。
fn teardown_on_shutdown(env: &Env) {
    let run = env.run_dir();
    shutdown_note(env, "daemon");
    if xr_vm_alive(env) {
        return;
    }
    crate::clash::stop_clash_mux(env);
    for file_path in [
        run.join("state"),
        run.join("ready_at"),
        run.join("crosvm.pid"),
        run.join("guest/guest/openclash.status"),
    ] {
        fsx::remove_logged(env, file_path);
    }
    routes_emergency_down(env);
    sys::wakelock_off(env);
    fsx::remove_logged(env, run.join("net.gw"));
}

/// `run/stopping` 残留时：仍在停止中则不动；虚拟机还在则只删标记；进程已不在时补全停止清理。
fn finish_stop(env: &Env) {
    let run = env.run_dir();
    if vm_stopping(env) {
        return;
    }
    if xr_vm_alive(env) {
        fsx::remove_logged(env, run.join("stopping"));
        return;
    }
    log::info(env, "daemon", "补全未完成的停止清理");
    crate::clash::stop_clash_mux(env);
    for path in [
        run.join("state"),
        run.join("ready_at"),
        run.join("crosvm.pid"),
        run.join("stopping"),
        run.join("guest/guest/openclash.status"),
        env.fuse_file(),
        env.boot_log(),
    ] {
        fsx::remove_logged(env, path);
    }
    routes_emergency_down(env);
    sys::wakelock_off(env);
    notify_cancel(env);
}

/// `crosvm` 异常退出后的紧急清理：关机中只做必要清理；否则停止分流、撤销网络、清理运行状态，并记 ERROR"异常退出"。
///
/// 停止流程未完成（`run/stopping` 残留且进程已不在）时补全清理。
pub(crate) fn emergency_teardown(env: &Env, why: &str) {
    let run = env.run_dir();
    if sys::shutting_down(env) {
        teardown_on_shutdown(env);
        return;
    }
    if run.join("stopping").is_file() {
        finish_stop(env);
        return;
    }
    let last = fs::read_to_string(run.join("teardown.last")).unwrap_or_default();
    if last.trim() != why {
        fsx::write_logged(env, run.join("teardown.last"), format!("{why}\n"));
        log::err(env, "daemon", &format!("异常退出：{why}"));
        diag::snap(env, &format!("异常：{why}"), 3);
    }
    if xr_vm_alive(env) {
        return;
    }
    crate::clash::stop_clash_mux(env);
    fsx::write_logged(env, run.join("last_error"), "服务意外退出\n");
    fsx::remove_logged(env, run.join("state"));
    fsx::remove_logged(env, run.join("ready_at"));
    fsx::remove_logged(env, run.join("crosvm.pid"));
    fsx::remove_logged(env, run.join("guest/guest/openclash.status"));
    routes_emergency_down(env);
    sys::wakelock_off(env);
    notify_cancel(env);
}

/// 网关规则残留需要撤销时返回 true：`net.gw` 存在且不在停止中，并且 `xr0` 无载波或虚拟机已不在。
#[must_use]
pub(crate) fn stale_gateway(env: &Env) -> bool {
    let run = env.run_dir();
    if !run.join("net.gw").is_file() || vm_stopping(env) {
        return false;
    }
    if xr_vm_alive(env) {
        let car = env.sysnet.join("xr0/carrier");
        if let Ok(carrier_text) = fs::read_to_string(car)
            && carrier_text.trim() == "0"
        {
            return true;
        }
        return false;
    }
    true
}
