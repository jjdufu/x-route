use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use rustix::process::Signal;

use crate::config::Config;
use crate::diag::diag_snap;
use crate::env::Env;
use crate::log::{log_err, log_info, unix_now};
use crate::net::common::NetCtx;
use crate::sys::{is_pid_running, proc_cmdline, send_signal, shutdown_note, sys_shutting_down};
use crate::traffic::acct_total;
use crate::vm::{run_net_hook, vm_running};

pub fn cgroup_escape(env: &Env, pid: &str) -> bool {
    let trimmed = pid.trim();
    if trimmed.is_empty() || !is_pid_running(env, trimmed) {
        return false;
    }
    let root = &env.cgroup_root;
    let mut v2_ok = false;
    for f in [root.join("cgroup.procs"), root.join("system/cgroup.procs")] {
        if fs::write(&f, format!("{trimmed}\n")).is_ok() {
            v2_ok = true;
        }
    }
    if !v2_ok {
        let uid0 = root.join("uid_0/cgroup.procs");
        let _ = fs::write(&uid0, format!("{trimmed}\n"));
    }
    for f in [
        "/dev/memcg/system/cgroup.procs",
        "/dev/cpuset/system-background/cgroup.procs",
        "/dev/cpuset/system-background/tasks",
        "/dev/cpuset/background/cgroup.procs",
        "/dev/cpuset/background/tasks",
        "/dev/stune/background/cgroup.procs",
        "/dev/stune/background/tasks",
    ] {
        let p = Path::new(f);
        if p.exists() {
            let _ = fs::write(p, format!("{trimmed}\n"));
        }
    }
    let oom = env.proc_base.join(trimmed).join("oom_score_adj");
    let _ = fs::write(oom, "-1000\n");
    true
}

pub fn cgroup_escape_tree(env: &Env, self_pid: u32) {
    let _ = cgroup_escape(env, &self_pid.to_string());
    let run = env.run_dir();
    for f in [
        "crosvm.pid",
        "net_watch.pid",
        "fuse_watch.pid",
        "watchdog.pid",
        "daemon.pid",
        "notifier.pid",
    ] {
        if let Ok(p) = fs::read_to_string(run.join(f)) {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                let _ = cgroup_escape(env, trimmed);
            }
        }
    }
}

pub fn cgroup_path(env: &Env, pid: &str) -> Option<String> {
    let content = fs::read_to_string(env.proc_base.join(pid.trim()).join("cgroup")).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("0::") {
            return Some(rest.to_string());
        }
        let mut parts = line.splitn(3, ':');
        let _ = parts.next();
        let _ = parts.next();
        if let Some(rest) = parts.next() {
            return Some(rest.to_string());
        }
    }
    None
}

pub fn cgroup_in_apps(env: &Env, pid: &str) -> bool {
    let Some(p) = cgroup_path(env, pid) else {
        return false;
    };
    p.starts_with("/apps/")
        || p.starts_with("apps/")
        || p.contains("/apps/uid_")
        || p.contains("/uid_") && p.contains("/pid_")
        || p.contains("suki")
}

pub fn xr_vm_alive(env: &Env) -> bool {
    if vm_running(env) {
        return true;
    }
    let pid = fs::read_to_string(env.run_dir().join("crosvm.pid")).unwrap_or_default();
    let p = pid.trim();
    if p.is_empty() || !env.proc_base.join(p).is_dir() {
        return false;
    }
    if !is_pid_running(env, p) {
        return false;
    }
    proc_cmdline(env, p).is_some_and(|c| c.contains("crosvm"))
}

pub fn vm_stopping(env: &Env) -> bool {
    let st = env.run_dir().join("stopping");
    if !st.is_file() {
        return false;
    }
    let sp = fs::read_to_string(st).unwrap_or_default();
    is_pid_running(env, sp.trim())
}

pub fn notify_state(env: &Env) -> &'static str {
    let s = fs::read_to_string(env.run_dir().join("state")).unwrap_or_default();
    match s.trim() {
        "stable" => "运行中",
        "ready" => "已就绪",
        _ => "启动中",
    }
}

pub fn notify_payload(env: &Env, cfg: &Config) -> String {
    let m = cfg.notify_traffic();
    let ctx = NetCtx::new(env, cfg);
    let [wu, wd, lu, ld] = acct_total(&ctx);
    format!("{} {m} {wu} {wd} {lu} {ld}", notify_state(env))
}

pub fn screen_on(env: &Env) -> bool {
    if let Some(s) = &env.screen_override {
        return s == "on";
    }
    if let Ok(out) = Command::new("dumpsys").arg("power").output() {
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            if let Some(pos) = line.find("mWakefulness=") {
                let rest = line
                    .get(pos.saturating_add("mWakefulness=".len())..)
                    .unwrap_or("");
                let word: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
                if matches!(word.as_str(), "Asleep" | "Dozing") {
                    return false;
                }
            }
        }
    }
    true
}

pub fn notifier_running(env: &Env) -> bool {
    let pid = fs::read_to_string(env.run_dir().join("notifier.pid")).unwrap_or_default();
    let p = pid.trim();
    if p.is_empty() || !is_pid_running(env, p) {
        return false;
    }
    proc_cmdline(env, p).is_some_and(|c| c.contains("xrnotify"))
}

fn notifier_exec(env: &Env, mode: &str) {
    let cp = format!("-Djava.class.path={}", env.notifier_jar.display());
    let _ = Command::new(&env.app_process)
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
}

pub fn notifier_start(env: &Env) -> bool {
    if notifier_running(env) {
        return true;
    }
    if !env.notifier_jar.is_file() {
        return false;
    }
    let run = env.run_dir();
    let _ = fs::remove_file(run.join("notify.req"));
    let _ = fs::remove_file(run.join("notify.ack"));
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
    let _ = fs::write(run.join("notifier.pid"), format!("{pid_s}\n"));
    let _ = cgroup_escape(env, &pid_s);
    true
}

fn notifier_req(env: &Env, payload: &str) -> bool {
    let run = env.run_dir();
    let tmp = run.join("notify.req.tmp");
    let dst = run.join("notify.req");
    fs::write(&tmp, format!("{payload}\n")).is_ok() && fs::rename(&tmp, &dst).is_ok()
}

fn notifier_wait(env: &Env, expected: &str) -> bool {
    let ack = env.run_dir().join("notify.ack");
    for _ in 0..env.notify_wait {
        if let Ok(a) = fs::read_to_string(&ack) {
            let trimmed = a.trim();
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

pub fn notifier_post(env: &Env, payload: &str) -> bool {
    if !notifier_start(env) || !notifier_req(env, payload) {
        return false;
    }
    notifier_wait(env, &format!("ok {payload}"))
}

pub fn notify_cancel(env: &Env) {
    let run = env.run_dir();
    let mut done = false;
    if notifier_running(env) {
        if notifier_req(env, "cancel") && notifier_wait(env, "ok cancel") {
            done = true;
        }
        if let Ok(p) = fs::read_to_string(run.join("notifier.pid")) {
            let _ = send_signal(p.trim(), Signal::TERM);
        }
    }
    if !done && env.notifier_jar.is_file() {
        notifier_exec(env, "cancel");
    }
    for f in [
        "notifier.pid",
        "notify.req",
        "notify.ack",
        "notify.on",
        "notify.last",
        "notify.ts",
    ] {
        let _ = fs::remove_file(run.join(f));
    }
}

pub fn notify_update(env: &Env) {
    let run = env.run_dir();
    if !xr_vm_alive(env) {
        if run.join("notify.on").is_file() || !run.join("notify.idle").is_file() {
            notify_cancel(env);
            let _ = fs::write(run.join("notify.idle"), "");
        }
        return;
    }
    let _ = fs::remove_file(run.join("notify.idle"));
    let mut force = false;
    if run.join("notify.force").is_file() {
        let _ = fs::remove_file(run.join("notify.force"));
        force = true;
    }
    if run.join("notify.on").is_file() && !notifier_running(env) {
        let _ = fs::remove_file(run.join("notify.on"));
    }
    let now = unix_now();
    let ts = fs::read_to_string(run.join("notify.ts"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if run.join("notify.on").is_file() && !force && now.saturating_sub(ts) < env.notify_interval {
        return;
    }
    let cfg = Config::load(env);
    let pl = notify_payload(env, &cfg);
    if run.join("notify.on").is_file() && !force {
        let last = fs::read_to_string(run.join("notify.last")).unwrap_or_default();
        if last.trim() == pl || !screen_on(env) {
            let _ = fs::write(run.join("notify.ts"), format!("{now}\n"));
            return;
        }
    }
    if notifier_post(env, &pl) {
        let _ = fs::write(run.join("notify.last"), format!("{pl}\n"));
        let _ = fs::write(run.join("notify.ts"), format!("{now}\n"));
        let _ = fs::write(run.join("notify.on"), "");
        let _ = fs::remove_file(run.join("notify.warned"));
        return;
    }
    let _ = fs::write(run.join("notify.ts"), format!("{now}\n"));
    if !run.join("notify.warned").is_file() {
        let ack = fs::read_to_string(run.join("notify.ack")).unwrap_or_default();
        let why = if ack.trim().is_empty() {
            let nlog = fs::read_to_string(run.join("notifier.log")).unwrap_or_default();
            let lines: Vec<&str> = nlog.lines().collect();
            let start = lines.len().saturating_sub(3);
            let joined = lines.get(start..).unwrap_or(&[]).join(" ");
            if joined.trim().is_empty() {
                "无回执".to_string()
            } else {
                joined
            }
        } else {
            ack.trim().to_string()
        };
        let short: String = why.chars().take(200).collect();
        log_err(env, "daemon", &format!("常驻通知发送失败：{short}"));
        let _ = fs::write(run.join("notify.warned"), "");
    }
}

pub const VMM_RUN_MARKER: &str = "=== xr crosvm start ";

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

pub fn vmm_has_guest_reset(vmm: &str) -> bool {
    let cur = vmm_last_run_slice(vmm);
    cur.contains("vcpu requested reset") || cur.contains("system reset event")
}

pub fn vm_guest_reset_pending(env: &Env) -> bool {
    if vm_stopping(env) || xr_vm_alive(env) || sys_shutting_down(env) {
        return false;
    }
    let st = fs::read_to_string(env.run_dir().join("state")).unwrap_or_default();
    if !matches!(st.trim(), "ready" | "stable") {
        return false;
    }
    fs::read_to_string(env.vmm_log()).is_ok_and(|s| vmm_has_guest_reset(&s))
}

pub fn vm_exit_reason(env: &Env) -> String {
    let p = fs::read_to_string(env.run_dir().join("crosvm.pid"))
        .unwrap_or_default()
        .trim()
        .to_string();
    if !p.is_empty() && is_pid_running(env, &p) {
        return "crosvm 仍在运行".to_string();
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
    "crosvm 已退出，日志未记录退出原因".to_string()
}

pub fn routes_emergency_down(env: &Env) {
    let _ = run_net_hook(env, "down");
}

pub fn emergency_teardown(env: &Env, why: &str) {
    let run = env.run_dir();
    if sys_shutting_down(env) {
        shutdown_note(env, "daemon");
        if xr_vm_alive(env) {
            return;
        }
        crate::clash::stop_clash_mux(env);
        for p in [
            run.join("state"),
            run.join("ready_at"),
            run.join("crosvm.pid"),
            run.join("guest/guest/openclash.status"),
        ] {
            let _ = fs::remove_file(p);
        }
        routes_emergency_down(env);
        crate::sys::wakelock_off();
        let _ = fs::remove_file(run.join("net.gw"));
        return;
    }
    if run.join("stopping").is_file() {
        if vm_stopping(env) {
            return;
        }
        if xr_vm_alive(env) {
            let _ = fs::remove_file(run.join("stopping"));
            return;
        }
        log_info(env, "daemon", "补全未完成的停止清理");
        crate::clash::stop_clash_mux(env);
        for p in [
            run.join("state"),
            run.join("ready_at"),
            run.join("crosvm.pid"),
            run.join("stopping"),
            run.join("guest/guest/openclash.status"),
            env.fuse_file(),
            env.boot_log(),
        ] {
            let _ = fs::remove_file(p);
        }
        routes_emergency_down(env);
        crate::sys::wakelock_off();
        notify_cancel(env);
        return;
    }
    let last = fs::read_to_string(run.join("teardown.last")).unwrap_or_default();
    if last.trim() != why {
        let _ = fs::write(run.join("teardown.last"), format!("{why}\n"));
        log_err(env, "daemon", &format!("异常退出：{why}"));
        diag_snap(env, &format!("异常：{why}"), 3);
    }
    if xr_vm_alive(env) {
        return;
    }
    crate::clash::stop_clash_mux(env);
    let _ = fs::write(run.join("last_error"), "服务意外退出\n");
    let _ = fs::remove_file(run.join("state"));
    let _ = fs::remove_file(run.join("ready_at"));
    let _ = fs::remove_file(run.join("crosvm.pid"));
    let _ = fs::remove_file(run.join("guest/guest/openclash.status"));
    routes_emergency_down(env);
    crate::sys::wakelock_off();
    notify_cancel(env);
}

pub fn stale_gateway(env: &Env) -> bool {
    let run = env.run_dir();
    if !run.join("net.gw").is_file() || vm_stopping(env) {
        return false;
    }
    if xr_vm_alive(env) {
        let car = env.sysnet.join("xr0/carrier");
        if let Ok(c) = fs::read_to_string(car) {
            if c.trim() == "0" {
                return true;
            }
        }
        return false;
    }
    true
}
