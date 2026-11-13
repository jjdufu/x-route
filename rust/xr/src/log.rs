use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Local;

use crate::config::{set_kv, Config};
use crate::env::{is_executable, Env};

const LOG_ROTATE_BYTES: u64 = 524_288;
const DEBUG_ROTATE_BYTES: u64 = 2_097_152;

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn time_hms() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

pub fn time_ymdhms_compact() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

pub fn time_full_tz() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S %z").to_string()
}

pub fn xr_now(env: &Env) -> String {
    if !env.fake_date_fail {
        let s = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        if !s.is_empty() {
            return s;
        }
    }
    if is_executable(&env.bb) {
        if let Ok(out) = Command::new(&env.bb).args(["date", "+%F %T"]).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    let up = fs::read_to_string(&env.proc_uptime)
        .ok()
        .and_then(|s| s.split_whitespace().next().map(ToString::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "?".to_string());
    format!("uptime+{up}s")
}

pub fn rotate_file(path: &Path, use_mv: bool) {
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
        let _ = fs::rename(path, &bak_path);
    } else {
        let _ = fs::copy(path, &bak_path);
        let _ = fs::File::create(path);
    }
}

pub fn rotate_debug(env: &Env) {
    let dlog = env.debug_log();
    let Ok(meta) = fs::metadata(&dlog) else {
        return;
    };
    if meta.is_file() && meta.len() > DEBUG_ROTATE_BYTES {
        let bak = env.log_dir().join("debug.log.1");
        let _ = fs::rename(&dlog, bak);
    }
}

pub fn rotate_all_logs(env: &Env) {
    rotate_file(&env.module_log(), true);
    rotate_debug(env);
    rotate_file(&env.vmm_log(), false);
    rotate_file(&env.console_log(), false);
    rotate_file(&env.ctl_log(), false);
}

fn append_line(path: &Path, line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

pub fn log_msg(env: &Env, level: &str, module: &str, msg: &str) {
    let _ = fs::create_dir_all(env.log_dir());
    let log_path = env.module_log();
    rotate_file(&log_path, true);
    let ts = xr_now(env);
    append_line(&log_path, &format!("{ts} [{level}] [{module}] {msg}"));
    let boot_log = env.boot_log();
    if boot_log.is_file() {
        let short_ts = ts.split_once(' ').map_or(ts.as_str(), |(_, rhs)| rhs);
        append_line(&boot_log, &format!("{short_ts} [{module}] {msg}"));
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
    append_line(&boot_log, &format!("{} [{module}] {msg}", time_hms()));
}

pub fn dlog(env: &Env, cfg: &Config, module: &str, msg: &str) {
    if !cfg.debug_on() {
        return;
    }
    let _ = fs::create_dir_all(env.log_dir());
    let pid = std::process::id();
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
    append_line(&env.debug_log(), &format!("{ts} [{pid}] [{module}] {msg}"));
}

pub fn debug_arm(env: &Env) {
    let cfg = Config::load(env);
    let until_file = env.debug_until_file();
    if cfg.debug_on() && cfg.debug_hours() != 0 {
        let secs = cfg.debug_hours().saturating_mul(3600);
        let until = unix_now().saturating_add(secs);
        let _ = fs::create_dir_all(env.config_dir());
        let _ = fs::write(&until_file, format!("{until}\n"));
    } else {
        let _ = fs::remove_file(&until_file);
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
        let _ = fs::remove_file(&until_file);
        log_info(env, "debug", "调试模式已到期，自动关闭");
    }
}

pub fn boot_clean_run(env: &Env) -> bool {
    let run = env.run_dir();
    if !run.is_dir() {
        return true;
    }
    let run_s = run.to_string_lossy();
    let pat_slash = format!(" {run_s}/");
    let pat_space = format!(" {run_s} ");
    if let Ok(mounts) = fs::read_to_string(&env.proc_mounts) {
        if mounts
            .lines()
            .any(|l| l.contains(&pat_slash) || l.contains(&pat_space))
        {
            log_err(env, "service", "run/ 下存在挂载点，跳过开机清理");
            return false;
        }
    }
    let _ = fs::create_dir_all(env.log_dir());
    let con = env.console_log();
    if fs::metadata(&con).is_ok_and(|m| m.is_file() && m.len() > 0) {
        let _ = fs::rename(&con, env.log_dir().join("console.prev.log"));
    }
    if let Ok(entries) = fs::read_dir(&run) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let _ = fs::remove_dir_all(&p);
            } else {
                let _ = fs::remove_file(&p);
            }
        }
    }
    true
}
