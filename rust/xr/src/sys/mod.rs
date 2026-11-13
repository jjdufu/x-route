pub mod exec;
pub mod spawn;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use rustix::fs::{flock, FlockOperation};
use rustix::process::{kill_process, test_kill_process, Pid, Signal};

use crate::env::Env;
use crate::log::log_info;

pub struct FileLock {
    _file: File,
}

impl FileLock {
    pub fn acquire(lock_path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = lock_path.parent() {
            let _ = fs::create_dir_all(parent);
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

pub struct DirLock {
    path: PathBuf,
}

impl DirLock {
    pub fn acquire(path: PathBuf, max_iters: u32, sleep_ms: u64, check_pid: bool) -> Self {
        let mut n = 0_u32;
        while fs::create_dir(&path).is_err() {
            let stale_pid = if check_pid {
                fs::read_to_string(path.join("pid"))
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .and_then(Pid::from_raw)
                    .is_some_and(|pid| test_kill_process(pid).is_err())
            } else {
                false
            };
            if stale_pid || n >= max_iters {
                let _ = fs::remove_dir_all(&path);
                continue;
            }
            thread::sleep(Duration::from_millis(sleep_ms));
            n = n.saturating_add(1);
        }
        if check_pid {
            let _ = fs::write(path.join("pid"), format!("{}\n", std::process::id()));
        }
        Self { path }
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
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
    if raw_pid <= 0 {
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
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
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
        let _ = fs::create_dir_all(env.run_dir());
        let _ = File::create(marker);
        return true;
    }
    false
}

pub fn shutdown_note(env: &Env, module: &str) {
    let noted = env.run_dir().join("shutdown.noted");
    if noted.is_file() {
        return;
    }
    let _ = fs::create_dir_all(env.run_dir());
    let _ = File::create(noted);
    let m = if module.is_empty() { "daemon" } else { module };
    log_info(env, m, "系统关机，服务随之停止");
}

pub fn wakelock_on() {
    if let Ok(mut f) = OpenOptions::new().write(true).open("/sys/power/wake_lock") {
        let _ = writeln!(f, "xrouter");
    }
}

pub fn wakelock_off() {
    if let Ok(mut f) = OpenOptions::new()
        .write(true)
        .open("/sys/power/wake_unlock")
    {
        let _ = writeln!(f, "xrouter");
    }
}

fn write_existing(path: &Path, text: &str) -> bool {
    if !path.is_file() {
        return false;
    }
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .is_ok()
}

pub fn cgroup_path(env: &Env, pid_str: &str) -> Option<String> {
    let cg_file = env.proc_base.join(pid_str.trim()).join("cgroup");
    let content = fs::read_to_string(cg_file).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("0::") {
            return Some(rest.to_string());
        }
        let mut parts = line.splitn(3, ':');
        let (_id, _ctrl, path) = (parts.next(), parts.next(), parts.next());
        if let Some(p) = path {
            return Some(p.to_string());
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
    let line = format!("{trimmed}\n");
    let mut v2_ok = false;
    for rel in ["cgroup.procs", "system/cgroup.procs"] {
        if write_existing(&env.cgroup_root.join(rel), &line) {
            v2_ok = true;
        }
    }
    if !v2_ok {
        let _ = write_existing(&env.cgroup_root.join("uid_0/cgroup.procs"), &line);
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
        let _ = write_existing(Path::new(legacy), &line);
    }
    let oom_path = env.proc_base.join(trimmed).join("oom_score_adj");
    let _ = write_existing(&oom_path, "-1000\n");
    true
}

pub fn cgroup_escape_tree(env: &Env, self_pid: Option<&str>) {
    let own = std::process::id().to_string();
    let target = self_pid.filter(|s| !s.is_empty()).unwrap_or(&own);
    let _ = cgroup_escape(env, target);
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
                let _ = cgroup_escape(env, trimmed);
            }
        }
    }
}
