pub mod exec;
pub(crate) mod fsx;
pub mod spawn;

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use rustix::fs::{FlockOperation, flock};
use rustix::process::{Pid, Signal, kill_process, test_kill_process};

use crate::env::Env;
use crate::log::log_info;

pub struct FileLock {
    _file: File,
}

impl FileLock {
    pub fn acquire(env: &Env, lock_path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = lock_path.parent() {
            fsx::mkdirs_logged(env, parent);
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        flock(&file, FlockOperation::LockExclusive)
            .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
        Ok(Self { _file: file })
    }
}

pub struct DirLock<'env> {
    /// 释放锁时记录失败用的环境。
    env: &'env Env,
    path: PathBuf,
}

impl<'env> DirLock<'env> {
    pub fn acquire(
        env: &'env Env,
        path: PathBuf,
        max_iters: u32,
        sleep_ms: u64,
        check_pid: bool,
    ) -> Self {
        if let Some(parent) = path.parent() {
            fsx::mkdirs_logged(env, parent);
        }
        let mut n = 0_u32;
        while fs::create_dir(&path).is_err() {
            if let Some(parent) = path.parent() {
                if !parent.exists() && fs::create_dir_all(parent).is_err() {
                    break;
                }
            }
            let stale_pid = if check_pid {
                fs::read_to_string(path.join("pid"))
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .and_then(Pid::from_raw)
                    .is_some_and(|pid| test_kill_process(pid).is_err())
            } else {
                false
            };
            if stale_pid {
                fsx::remove_tree_logged(env, &path);
                continue;
            }
            if n >= max_iters {
                force_take_dir_lock(env, &path);
                break;
            }
            thread::sleep(Duration::from_millis(sleep_ms));
            n = n.saturating_add(1);
        }
        if check_pid && path.is_dir() {
            fsx::write_logged(
                env,
                &path.join("pid"),
                format!("{}\n", std::process::id()).as_bytes(),
            );
        }
        Self { env, path }
    }
}

/// 等待目录锁超时后强行接管：删掉旧锁目录再新建。失败时计数并记录，调用方照常继续（与原行为一致）。
fn force_take_dir_lock(env: &Env, path: &Path) {
    fsx::remove_tree_logged(env, path);
    if let Err(err) = fs::create_dir(path) {
        fsx::record_failure(env, "mkdir", path, &err);
    }
}

impl Drop for DirLock<'_> {
    fn drop(&mut self) {
        fsx::remove_tree_logged(self.env, &self.path);
    }
}

pub fn is_zombie_stat(stat_content: &str) -> bool {
    stat_content.contains(") Z ") || stat_content.contains(") X ") || stat_content.contains(") x ")
}

pub fn is_pid_running(env: &Env, pid_str: &str) -> bool {
    let trimmed = pid_str.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Ok(raw_pid) = trimmed.parse::<i32>() else {
        return false;
    };
    if raw_pid <= 0_i32 {
        return false;
    }
    if env.proc_base != Path::new("/proc") {
        let fake_dir = env.proc_base.join(trimmed);
        if fake_dir.is_dir() {
            if let Ok(st) = fs::read_to_string(fake_dir.join("stat")) {
                if is_zombie_stat(&st) {
                    return false;
                }
            }
            return true;
        }
    }
    let Some(pid) = Pid::from_raw(raw_pid) else {
        return false;
    };
    if test_kill_process(pid).is_err() {
        return false;
    }
    let stat_path = PathBuf::from(format!("/proc/{raw_pid}/stat"));
    if let Ok(st) = fs::read_to_string(stat_path) {
        if is_zombie_stat(&st) {
            return false;
        }
    }
    true
}

pub fn send_signal(pid_str: &str, sig: Signal) -> bool {
    let Ok(raw_pid) = pid_str.trim().parse::<i32>() else {
        return false;
    };
    let Some(pid) = Pid::from_raw(raw_pid) else {
        return false;
    };
    kill_process(pid, sig).is_ok()
}

/// 发送信号；失败（进程已退出或无权限）时（调试开关打开）写调试日志，调用方照常继续。
pub(crate) fn send_signal_logged(env: &Env, pid_str: &str, sig: Signal) {
    if !send_signal(pid_str, sig) {
        crate::log::dlog_env(
            env,
            "proc",
            &format!(
                "向 {} 发送 {sig:?} 未成功（进程可能已退出）",
                pid_str.trim()
            ),
        );
    }
}

pub fn proc_cmdline(env: &Env, pid_str: &str) -> Option<String> {
    let trimmed = pid_str.trim();
    if trimmed.is_empty() {
        return None;
    }
    let p1 = env.proc_base.join(trimmed).join("cmdline");
    let bytes = fs::read(&p1)
        .or_else(|_| fs::read(format!("/proc/{trimmed}/cmdline")))
        .ok()?;
    let mapped: Vec<u8> = bytes
        .into_iter()
        .map(|b| if b == 0 { b' ' } else { b })
        .collect();
    Some(String::from_utf8_lossy(&mapped).to_string())
}

pub fn proc_uptime_secs_for_pid(env: &Env, pid_str: &str) -> u64 {
    let trimmed = pid_str.trim();
    let stat_path = env.proc_base.join(trimmed).join("stat");
    let Ok(stat_s) = fs::read_to_string(&stat_path)
        .or_else(|_| fs::read_to_string(format!("/proc/{trimmed}/stat")))
    else {
        return 0;
    };
    let Some((_, after_comm)) = stat_s.rsplit_once(") ") else {
        return 0;
    };
    let Some(start_ticks) = after_comm
        .split_whitespace()
        .nth(19)
        .and_then(|s| s.parse::<u64>().ok())
    else {
        return 0;
    };
    let Ok(up_s) =
        fs::read_to_string(&env.proc_uptime).or_else(|_| fs::read_to_string("/proc/uptime"))
    else {
        return 0;
    };
    let Some(now_secs) = up_s
        .split_whitespace()
        .next()
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse::<u64>().ok())
    else {
        return 0;
    };
    let started_secs = start_ticks / 100;
    now_secs.saturating_sub(started_secs)
}

pub fn getprop(name: &str) -> String {
    Command::new("getprop")
        .arg(name)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

pub fn ce_ready() -> bool {
    getprop("sys.user.0.ce_available") == "true"
}

pub fn wait_boot_completed() {
    while getprop("sys.boot_completed") != "1" {
        thread::sleep(Duration::from_secs(2));
    }
}

pub fn sys_shutting_down(env: &Env) -> bool {
    let marker = env.run_dir().join("shutdown");
    if marker.is_file() {
        return true;
    }
    if !getprop("sys.powerctl").is_empty() || !getprop("sys.shutdown.requested").is_empty() {
        fsx::mkdirs_logged(env, &env.run_dir());
        fsx::touch_logged(env, &marker);
        return true;
    }
    false
}

pub fn shutdown_note(env: &Env, module: &str) {
    let noted = env.run_dir().join("shutdown.noted");
    if noted.is_file() {
        return;
    }
    fsx::mkdirs_logged(env, &env.run_dir());
    fsx::touch_logged(env, &noted);
    let m = if module.is_empty() { "daemon" } else { module };
    log_info(env, m, "系统关机，服务随之停止");
}

pub fn wakelock_on(env: &Env) {
    wakelock_write(env, Path::new("/sys/power/wake_lock"));
}

pub fn wakelock_off(env: &Env) {
    wakelock_write(env, Path::new("/sys/power/wake_unlock"));
}

/// 向内核唤醒锁节点写入本模块的锁名。节点打不开（非 Android 或无权限）时静默跳过，与原行为一致；
/// 打开后写入失败则计数并记录。
fn wakelock_write(env: &Env, node: &Path) {
    if let Ok(mut file) = OpenOptions::new().write(true).open(node)
        && let Err(err) = writeln!(file, "xrouter")
    {
        fsx::record_failure(env, "write", node, &err);
    }
}

/// 文件存在时覆盖写入 `text`；文件不存在时返回 `None`（不创建）。
fn write_if_exists(path: &Path, text: &str) -> Option<std::io::Result<()>> {
    if !path.is_file() {
        return None;
    }
    Some(
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .and_then(|mut file| file.write_all(text.as_bytes())),
    )
}

fn write_existing(path: &Path, text: &str) -> bool {
    matches!(write_if_exists(path, text), Some(Ok(())))
}

/// 尽力写入已存在的控制文件：文件不存在时跳过（不同内核的节点不同，属正常情况）；
/// 存在但写入失败时计数并记录。
fn write_existing_logged(env: &Env, path: &Path, text: &str) {
    if let Some(Err(err)) = write_if_exists(path, text) {
        fsx::record_failure(env, "write", path, &err);
    }
}

pub fn cgroup_path(env: &Env, pid_str: &str) -> Option<String> {
    let cg_file = env.proc_base.join(pid_str.trim()).join("cgroup");
    let content = fs::read_to_string(cg_file).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("0::") {
            return Some(rest.to_owned());
        }
        let mut parts = line.splitn(3, ':');
        let (_id, _ctrl, path) = (parts.next(), parts.next(), parts.next());
        if let Some(p) = path {
            return Some(p.to_owned());
        }
    }
    None
}

pub fn cgroup_in_apps(env: &Env, pid_str: &str) -> bool {
    let Some(p) = cgroup_path(env, pid_str) else {
        return false;
    };
    p.starts_with("/apps/")
        || p.starts_with("apps/")
        || p.contains("/apps/uid_")
        || p.contains("suki")
        || is_uid_pid_cgroup(&p)
}

fn is_uid_pid_cgroup(path: &str) -> bool {
    let segs: Vec<&str> = path.split('/').collect();
    for (idx, seg) in segs.iter().enumerate() {
        if seg.starts_with("uid_") {
            if let Some(next) = segs.get(idx.saturating_add(1)) {
                if next.starts_with("pid_") {
                    return true;
                }
            }
        }
    }
    false
}

pub fn cgroup_escape(env: &Env, pid_str: &str) -> bool {
    let trimmed = pid_str.trim();
    if trimmed.is_empty() || !is_pid_running(env, trimmed) {
        return false;
    }
    escape_pid(env, trimmed);
    true
}

/// 与 [`cgroup_escape`] 相同，但不关心进程是否在运行（不在运行时什么都不做）。
fn escape_if_running(env: &Env, pid_str: &str) {
    let trimmed = pid_str.trim();
    if !trimmed.is_empty() && is_pid_running(env, trimmed) {
        escape_pid(env, trimmed);
    }
}

/// 把已确认在运行的进程移出应用 cgroup，并调低其 OOM 优先级。
fn escape_pid(env: &Env, trimmed: &str) {
    let line = format!("{trimmed}\n");
    let mut v2_ok = false;
    for rel in ["cgroup.procs", "system/cgroup.procs"] {
        if write_existing(&env.cgroup_root.join(rel), &line) {
            v2_ok = true;
        }
    }
    if !v2_ok {
        let uid0 = env.cgroup_root.join("uid_0/cgroup.procs");
        write_existing_logged(env, &uid0, &line);
    }
    for legacy in [
        "/dev/memcg/system/cgroup.procs",
        "/dev/cpuset/system-background/cgroup.procs",
        "/dev/cpuset/system-background/tasks",
        "/dev/cpuset/background/cgroup.procs",
        "/dev/cpuset/background/tasks",
        "/dev/stune/background/cgroup.procs",
        "/dev/stune/background/tasks",
    ] {
        write_existing_logged(env, Path::new(legacy), &line);
    }
    let oom_path = env.proc_base.join(trimmed).join("oom_score_adj");
    write_existing_logged(env, &oom_path, "-1000\n");
}

pub fn cgroup_escape_tree(env: &Env, self_pid: Option<&str>) {
    let own = std::process::id().to_string();
    let target = self_pid.filter(|s| !s.is_empty()).unwrap_or(&own);
    escape_if_running(env, target);
    let run = env.run_dir();
    for name in [
        "crosvm.pid",
        "net_watch.pid",
        "fuse_watch.pid",
        "watchdog.pid",
        "daemon.pid",
        "notifier.pid",
    ] {
        if let Ok(p) = fs::read_to_string(run.join(name)) {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                escape_if_running(env, trimmed);
            }
        }
    }
}
