use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Local;

use crate::config::{Config, set_kv};
use crate::env::{Env, is_executable};
use crate::sys::fsx;

#[cfg(test)]
pub(crate) mod tests;

const LOG_ROTATE_BYTES: u64 = 524_288;
const DEBUG_ROTATE_BYTES: u64 = 2_097_152;

/// 尽力而为操作的失败计数（文件、命令、日志自身）。只增不减，进程内有效。
#[derive(Debug)]
pub(crate) struct Failures {
    /// 文件操作失败次数。
    pub(crate) fs: AtomicU64,
    /// 外部命令非 0 退出或无法执行的次数（只计 `xr_run` 发起的），以及进程设置类系统调用的失败次数。
    pub(crate) exec: AtomicU64,
    /// 日志行（以及转发给终端的命令输出）写入失败次数。
    pub(crate) log: AtomicU64,
}

/// 全局失败计数。
pub(crate) static FAILURES: Failures = Failures {
    fs: AtomicU64::new(0),
    exec: AtomicU64::new(0),
    log: AtomicU64::new(0),
};

/// 给 `counter` 记一次失败。计数到 `u64::MAX` 后保持上限，不回绕到 0。
pub(crate) fn note_failure(counter: &AtomicU64) {
    let before = counter.fetch_add(1, Ordering::Relaxed);
    if before == u64::MAX {
        counter.store(u64::MAX, Ordering::Relaxed);
    }
}

/// 记录一次进程设置类系统调用（`setsid`、`setrlimit`、信号处理注册）的失败：计数到 `FAILURES.exec`，
/// 调试开关打开时写调试日志。调用方继续执行。
pub(crate) fn record_setup_failure(env: &Env, what: &str, err: &dyn std::fmt::Display) {
    note_failure(&FAILURES.exec);
    dlog_env(env, "proc", &format!("{what} 失败：{err}"));
}

/// 读取一个返回码：非 0 时（调试开关打开）写一行调试日志，调用方照常继续。
pub(crate) fn note_rc(env: &Env, what: &str, rc: i32) {
    if rc != 0_i32 {
        dlog_env(env, "rc", &format!("{what} 返回 {rc}"));
    }
}

/// 读取一个成败标志：失败时（调试开关打开）写一行调试日志，调用方照常继续。
pub(crate) fn note_ok(env: &Env, what: &str, ok: bool) {
    if !ok {
        dlog_env(env, "rc", &format!("{what} 未成功"));
    }
}

/// 诊断包 `summary.txt` 的失败计数行：三个计数全为 0 时返回空串，否则返回带换行的一行。
pub(crate) fn failures_summary_line() -> String {
    format_failures_line(
        FAILURES.fs.load(Ordering::Relaxed),
        FAILURES.exec.load(Ordering::Relaxed),
        FAILURES.log.load(Ordering::Relaxed),
    )
}

/// 按给定计数生成失败计数行；全为 0 时返回空串。
fn format_failures_line(fs_n: u64, exec_n: u64, log_n: u64) -> String {
    if fs_n == 0 && exec_n == 0 && log_n == 0 {
        return String::new();
    }
    format!("best_effort_failures fs={fs_n} exec={exec_n} log={log_n}\n")
}

#[must_use]
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[must_use]
pub fn time_hms() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

#[must_use]
pub fn time_ymdhms_compact() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

#[must_use]
pub fn time_full_tz() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S %z").to_string()
}

#[must_use]
pub fn xr_now(env: &Env) -> String {
    if !env.fake_date_fail {
        let s = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        if !s.is_empty() {
            return s;
        }
    }
    if is_executable(&env.bb)
        && let Ok(out) = Command::new(&env.bb).args(["date", "+%F %T"]).output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !s.is_empty() {
            return s;
        }
    }
    let up = fs::read_to_string(&env.proc_uptime)
        .ok()
        .and_then(|s| s.split_whitespace().next().map(str::to_owned))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "?".to_owned());
    format!("uptime+{up}s")
}

pub fn rotate_file(env: &Env, path: &Path, use_mv: bool) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if !meta.is_file() || meta.len() <= LOG_ROTATE_BYTES {
        return;
    }
    let mut bak = path.as_os_str().to_os_string();
    bak.push(".1");
    let bak_path = std::path::PathBuf::from(bak);
    if use_mv {
        fsx::rename_logged(env, path, &bak_path);
    } else {
        fsx::copy_logged(env, path, &bak_path);
        fsx::touch_logged(env, path);
    }
}

pub fn rotate_debug(env: &Env) {
    let dlog = env.debug_log();
    let Ok(meta) = fs::metadata(&dlog) else {
        return;
    };
    if meta.is_file() && meta.len() > DEBUG_ROTATE_BYTES {
        let bak = env.log_dir().join("debug.log.1");
        fsx::rename_logged(env, &dlog, &bak);
    }
}

pub fn rotate_all_logs(env: &Env) {
    rotate_file(env, &env.module_log(), true);
    rotate_debug(env);
    rotate_file(env, &env.vmm_log(), false);
    rotate_file(env, &env.console_log(), false);
    rotate_file(env, &env.ctl_log(), false);
}

/// 以追加方式写一行到日志文件（文件不存在时创建）。
///
/// # Errors
///
/// 打开或写入失败时返回对应的 I/O 错误，由调用方计数。
pub(crate) fn append_line(path: &Path, line: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

/// 写一行日志；失败时只计数（`FAILURES.log`），不再尝试写别的日志，避免递归。
fn append_line_counted(path: &Path, line: &str) {
    if append_line(path, line).is_err() {
        note_failure(&FAILURES.log);
    }
}

/// 日志目录的创建：失败时只计数（`FAILURES.fs`），不写调试日志（调试日志也在该目录下）。
fn ensure_log_dir(dir: &Path) {
    if fs::create_dir_all(dir).is_err() {
        note_failure(&FAILURES.fs);
    }
}

pub fn log_msg(env: &Env, level: &str, module: &str, msg: &str) {
    ensure_log_dir(&env.log_dir());
    let log_path = env.module_log();
    rotate_file(env, &log_path, true);
    let ts = xr_now(env);
    append_line_counted(&log_path, &format!("{ts} [{level}] [{module}] {msg}"));
    let boot_log = env.boot_log();
    if boot_log.is_file() {
        let short_ts = ts.split_once(' ').map_or(ts.as_str(), |(_, rhs)| rhs);
        append_line_counted(&boot_log, &format!("{short_ts} [{module}] {msg}"));
    }
}

pub fn log_info(env: &Env, module: &str, msg: &str) {
    log_msg(env, "INFO", module, msg);
}

pub fn log_err(env: &Env, module: &str, msg: &str) {
    log_msg(env, "ERROR", module, msg);
}

pub fn blog(env: &Env, module: &str, msg: &str) {
    let boot_log = env.boot_log();
    if !boot_log.is_file() {
        return;
    }
    append_line_counted(&boot_log, &format!("{} [{module}] {msg}", time_hms()));
}

pub fn dlog(env: &Env, cfg: &Config, module: &str, msg: &str) {
    if !cfg.debug_on() {
        return;
    }
    ensure_log_dir(&env.log_dir());
    let pid = std::process::id();
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
    append_line_counted(&env.debug_log(), &format!("{ts} [{pid}] [{module}] {msg}"));
}

/// 在没有现成 `Config` 的失败分支里写调试日志：先读配置，调试开关关闭时什么都不写。
///
/// 只应在失败路径调用；成功路径不读配置。
pub(crate) fn dlog_env(env: &Env, module: &str, msg: &str) {
    let cfg = Config::load(env);
    dlog(env, &cfg, module, msg);
}

pub fn debug_arm(env: &Env) {
    let cfg = Config::load(env);
    let until_file = env.debug_until_file();
    if cfg.debug_on() && cfg.debug_hours() != 0 {
        let secs = cfg.debug_hours().saturating_mul(3600);
        let until = unix_now().saturating_add(secs);
        fsx::mkdirs_logged(env, env.config_dir());
        fsx::write_logged(env, &until_file, format!("{until}\n").as_bytes());
    } else {
        fsx::remove_logged(env, &until_file);
    }
}

pub fn debug_expire(env: &Env) {
    let cfg = Config::load(env);
    if !cfg.debug_on() {
        return;
    }
    let until_file = env.debug_until_file();
    let Ok(raw) = fs::read_to_string(&until_file) else {
        return;
    };
    let trimmed = raw.trim();
    let Ok(until) = trimmed.parse::<u64>() else {
        return;
    };
    if until == 0 || unix_now() < until {
        return;
    }
    if set_kv(env, "XR_DEBUG", "0") {
        fsx::remove_logged(env, &until_file);
        log_info(env, "debug", "调试模式已到期，自动关闭");
    }
}

#[must_use]
pub fn boot_clean_run(env: &Env) -> bool {
    let run = env.run_dir();
    if !run.is_dir() {
        return true;
    }
    let run_s = run.to_string_lossy();
    let pat_slash = format!(" {run_s}/");
    let pat_space = format!(" {run_s} ");
    if let Ok(mounts) = fs::read_to_string(&env.proc_mounts)
        && mounts
            .lines()
            .any(|l| l.contains(&pat_slash) || l.contains(&pat_space))
    {
        log_err(env, "service", "run/ 下存在挂载点，跳过开机清理");
        return false;
    }
    fsx::mkdirs_logged(env, env.log_dir());
    let con = env.console_log();
    if fs::metadata(&con).is_ok_and(|m| m.is_file() && m.len() > 0) {
        fsx::rename_logged(env, &con, env.log_dir().join("console.prev.log"));
    }
    if let Ok(entries) = fs::read_dir(&run) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                fsx::remove_tree_logged(env, &p);
            } else {
                fsx::remove_logged(env, &p);
            }
        }
    }
    true
}
