use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::config::Config;
use crate::env::{Env, is_executable};
use crate::log::log_info;

#[derive(Debug, Clone)]
pub struct NetCtx<'a> {
    pub env: &'a Env,
    pub cfg: &'a Config,
}

impl<'a> NetCtx<'a> {
    pub fn new(env: &'a Env, cfg: &'a Config) -> Self {
        Self { env, cfg }
    }

    pub fn ip(&self, args: &[&str]) -> crate::sys::exec::ExecResult {
        crate::sys::exec::xr_exec(self.env, self.cfg, &self.env.sysbin_cmd("ip"), args, true)
    }

    pub fn ip6(&self, args: &[&str]) -> crate::sys::exec::ExecResult {
        let mut full = Vec::with_capacity(args.len().saturating_add(1));
        full.push("-6");
        full.extend_from_slice(args);
        crate::sys::exec::xr_exec(self.env, self.cfg, &self.env.sysbin_cmd("ip"), &full, true)
    }

    pub fn ndc(&self, args: &[&str]) -> crate::sys::exec::ExecResult {
        crate::sys::exec::xr_exec(self.env, self.cfg, &self.env.sysbin_cmd("ndc"), args, true)
    }

    pub fn ndc_ok(&self, args: &[&str]) -> bool {
        let res = self.ndc(args);
        res.stdout.lines().any(|l| l.starts_with("200 "))
            || res.stderr.lines().any(|l| l.starts_with("200 "))
    }

    pub fn ipt(&self, args: &[&str]) -> crate::sys::exec::ExecResult {
        let mut full = Vec::with_capacity(args.len().saturating_add(1));
        full.push("-w");
        full.extend_from_slice(args);
        crate::sys::exec::xr_exec(
            self.env,
            self.cfg,
            &self.env.sysbin_cmd("iptables"),
            &full,
            true,
        )
    }

    pub fn ip6t(&self, args: &[&str]) -> crate::sys::exec::ExecResult {
        let mut full = Vec::with_capacity(args.len().saturating_add(1));
        full.push("-w");
        full.extend_from_slice(args);
        crate::sys::exec::xr_exec(
            self.env,
            self.cfg,
            &self.env.sysbin_cmd("ip6tables"),
            &full,
            true,
        )
    }

    pub fn sysw(&self, path: &Path, val: &str) -> bool {
        let is_mock = self.env.proc_dir != Path::new("/proc");
        if is_mock {
            if let Some(p) = path.parent() {
                let _ = fs::create_dir_all(p);
            }
        }
        let ok = OpenOptions::new()
            .write(true)
            .create(is_mock)
            .truncate(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{val}"))
            .is_ok();
        let proc_prefix = format!(
            "{}/",
            self.env.proc_dir.to_string_lossy().trim_end_matches('/')
        );
        let path_s = path.to_string_lossy();
        let short = path_s.strip_prefix(&proc_prefix).unwrap_or(&path_s);
        let rc = i32::from(!ok);
        crate::log::dlog(
            self.env,
            self.cfg,
            "sysctl",
            &format!("rc={rc} {short} = {val}"),
        );
        ok
    }

    pub fn lnt(&self) -> &'static str {
        let mode = fs::read_to_string(self.env.run_dir().join("net.mode")).unwrap_or_default();
        if mode.trim() == "ndc" {
            "local_network"
        } else {
            "97"
        }
    }
}

pub fn diag_snap_cmd_available(env: &Env) -> bool {
    is_executable(&env.moddir.join("bin/xr")) || std::env::current_exe().is_ok()
}

pub fn trigger_diag_snap_bg(env: &Env, cfg: &Config, reason: &str) {
    if !cfg.debug_on() {
        return;
    }
    let exe = env.self_exe();
    let _ = Command::new(exe)
        .args(["internal", "diag-snap", reason])
        .env("MODDIR", &env.moddir)
        .env("XR_DATA", &env.xr_data)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TrafficCounters {
    pub up: u64,
    pub down: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrafficSummary {
    pub wan: TrafficCounters,
    pub lan: TrafficCounters,
    pub total: TrafficCounters,
    pub since: u64,
    pub counting: bool,
    pub ipv6: bool,
}

pub fn read_quad(path: &Path) -> [u64; 4] {
    let Ok(content) = fs::read_to_string(path) else {
        return [0; 4];
    };
    let Some(first_line) = content.lines().next() else {
        return [0; 4];
    };
    let mut vals = [0_u64; 4];
    let mut parts = first_line.split_whitespace();
    for slot in &mut vals {
        if let Some(tok) = parts.next() {
            if tok.bytes().all(|b| b.is_ascii_digit()) {
                *slot = tok.parse::<u64>().unwrap_or(0);
            }
        }
    }
    vals
}

pub fn write_quad(path: &Path, vals: [u64; 4]) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let pid = std::process::id();
    let tmp = path.with_extension(format!("tmp.{pid}"));
    let line = format!("{} {} {} {}\n", vals[0], vals[1], vals[2], vals[3]);
    if fs::write(&tmp, line).is_ok() {
        let _ = fs::rename(&tmp, path);
    } else {
        let _ = fs::remove_file(&tmp);
    }
}

pub fn combine_quad_add(a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
    [
        a[0].saturating_add(b[0]),
        a[1].saturating_add(b[1]),
        a[2].saturating_add(b[2]),
        a[3].saturating_add(b[3]),
    ]
}

pub fn combine_quad_max(a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// 1024 进制、一位小数的人类可读字节数（与 `notifier` 的 `Traffic.human`、`WebUI` 的 `fmtBytes` 同口径）。
///
/// 整数实现，结果与旧的浮点实现（`bytes as f64` 后逐级除以 1024、`{:.1}` 格式化）逐字节一致：
/// 先按 `f64` 的 53 位有效数字舍入，再按十分位"四舍六入五成双"舍入。
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024_u64 {
        return format!("{bytes} B");
    }
    let value = as_f64_value(bytes);
    let (idx, div) = unit_of(value);
    let tenths = round_tenths(value, div);
    let unit = UNITS.get(idx).copied().unwrap_or("TB");
    let whole = tenths.checked_div(10_u128).unwrap_or(0_u128);
    let frac = tenths.checked_rem(10_u128).unwrap_or(0_u128);
    format!("{whole}.{frac} {unit}")
}

/// 返回 `bytes` 转成 `f64` 后表示的精确整数值（就近舍入、平局取偶，与 `as f64` 相同）。
fn as_f64_value(bytes: u64) -> u128 {
    let width = 64_u32.saturating_sub(bytes.leading_zeros());
    let shift = width.saturating_sub(53_u32);
    if shift == 0 {
        return u128::from(bytes);
    }
    let mantissa = bytes.checked_shr(shift).unwrap_or(0_u64);
    let low_mask = 1_u64
        .checked_shl(shift)
        .map_or(u64::MAX, |bit| bit.saturating_sub(1_u64));
    let rest = bytes & low_mask;
    let half = 1_u64
        .checked_shl(shift.saturating_sub(1_u32))
        .unwrap_or(0_u64);
    let round_up = rest > half || (rest == half && mantissa & 1_u64 == 1_u64);
    let kept = if round_up {
        u128::from(mantissa).saturating_add(1_u128)
    } else {
        u128::from(mantissa)
    };
    kept.checked_shl(shift).unwrap_or(u128::MAX)
}

/// 找到 `value` 对应的单位下标（最多到 TB）与该单位的字节数。
const fn unit_of(value: u128) -> (usize, u128) {
    let mut idx = 0_usize;
    let mut div = 1_u128;
    while idx < 4_usize && value >= div.saturating_mul(1024_u128) {
        div = div.saturating_mul(1024_u128);
        idx = idx.saturating_add(1);
    }
    (idx, div)
}

/// `value / div` 舍入到十分位后的"十分之一"个数（平局取偶，与 `{:.1}` 相同）。
fn round_tenths(value: u128, div: u128) -> u128 {
    let scaled = value.saturating_mul(10_u128);
    let quot = scaled.checked_div(div).unwrap_or(0_u128);
    let rem = scaled.checked_rem(div).unwrap_or(0_u128);
    let twice = rem.saturating_mul(2_u128);
    let round_up = twice > div || (twice == div && quot & 1_u128 == 1_u128);
    if round_up {
        quot.saturating_add(1_u128)
    } else {
        quot
    }
}

pub fn log_net_mode_info(env: &Env, msg: &str) {
    log_info(env, "net", msg);
}
