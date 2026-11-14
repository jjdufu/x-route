//! 网络操作的公共部分：执行 `ip`/`iptables`/`ndc` 的上下文、内核参数写入、流量四元组文件与字节数格式化。

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::config::Config;
use crate::env::Env;
use crate::log;
use crate::sys::fsx;

/// 一次网络操作的上下文：运行环境与当次读取的配置。
#[derive(Debug, Clone)]
pub struct NetCtx<'a> {
    /// 运行环境。
    pub(crate) env: &'a Env,
    /// 当次读取的配置（决定调试日志等行为）。
    pub(crate) cfg: &'a Config,
}

impl<'a> NetCtx<'a> {
    /// 由运行环境和配置构造上下文。
    #[must_use]
    pub const fn new(env: &'a Env, cfg: &'a Config) -> Self {
        Self { env, cfg }
    }

    /// 执行 `ip`（经统一执行入口，转发标准错误）。
    #[must_use]
    pub(crate) fn ip(&self, args: &[&str]) -> crate::sys::exec::Output {
        crate::sys::exec::run_xr(self.env, self.cfg, &self.env.sysbin_cmd("ip"), args, true)
    }

    /// 执行 `ip -6`。
    #[must_use]
    pub(crate) fn ip6(&self, args: &[&str]) -> crate::sys::exec::Output {
        let mut full = Vec::with_capacity(args.len().saturating_add(1));
        full.push("-6");
        full.extend_from_slice(args);
        crate::sys::exec::run_xr(self.env, self.cfg, &self.env.sysbin_cmd("ip"), &full, true)
    }

    /// 执行 `ndc`。
    #[must_use]
    pub(crate) fn ndc(&self, args: &[&str]) -> crate::sys::exec::Output {
        crate::sys::exec::run_xr(self.env, self.cfg, &self.env.sysbin_cmd("ndc"), args, true)
    }

    /// 执行 `ndc`，输出中有以 `200 ` 开头的行（命令成功）时返回 true。
    #[must_use]
    pub(crate) fn ndc_ok(&self, args: &[&str]) -> bool {
        let res = self.ndc(args);
        res.stdout.lines().any(|row| row.starts_with("200 "))
            || res.stderr.lines().any(|line| line.starts_with("200 "))
    }

    /// 执行 `iptables -w`。
    #[must_use]
    pub(crate) fn ipt(&self, args: &[&str]) -> crate::sys::exec::Output {
        let mut full = Vec::with_capacity(args.len().saturating_add(1));
        full.push("-w");
        full.extend_from_slice(args);
        crate::sys::exec::run_xr(
            self.env,
            self.cfg,
            &self.env.sysbin_cmd("iptables"),
            &full,
            true,
        )
    }

    /// 执行 `ip6tables -w`。
    #[must_use]
    pub(crate) fn ip6t(&self, args: &[&str]) -> crate::sys::exec::Output {
        let mut full = Vec::with_capacity(args.len().saturating_add(1));
        full.push("-w");
        full.extend_from_slice(args);
        crate::sys::exec::run_xr(
            self.env,
            self.cfg,
            &self.env.sysbin_cmd("ip6tables"),
            &full,
            true,
        )
    }

    /// 与 [`Self::ip`] 相同，只关心成败：失败时计数，不返回结果。
    pub(crate) fn ip_run(&self, args: &[&str]) {
        crate::sys::exec::note_exec_result(&self.ip(args));
    }

    /// 与 [`Self::ip6`] 相同，只关心成败：失败时计数，不返回结果。
    pub(crate) fn ip6_run(&self, args: &[&str]) {
        crate::sys::exec::note_exec_result(&self.ip6(args));
    }

    /// 与 [`Self::ipt`] 相同，只关心成败：失败时计数，不返回结果。
    pub(crate) fn ipt_run(&self, args: &[&str]) {
        crate::sys::exec::note_exec_result(&self.ipt(args));
    }

    /// 与 [`Self::ip6t`] 相同，只关心成败：失败时计数，不返回结果。
    pub(crate) fn ip6t_run(&self, args: &[&str]) {
        crate::sys::exec::note_exec_result(&self.ip6t(args));
    }

    /// 与 [`Self::ndc`] 相同，只关心成败：失败时计数，不返回结果。
    pub(crate) fn ndc_run(&self, args: &[&str]) {
        crate::sys::exec::note_exec_result(&self.ndc(args));
    }

    /// 向内核参数文件写入一行 `val`，成功返回 true；调试开关打开时记一行 `sysctl` 调试日志。
    ///
    /// 测试把 `proc_dir` 指向假目录时会先创建父目录并允许创建文件。
    #[must_use]
    pub(crate) fn sysw(&self, path: &Path, val: &str) -> bool {
        let is_mock = self.env.proc_dir != Path::new("/proc");
        if is_mock && let Some(file_path) = path.parent() {
            fsx::mkdirs_logged(self.env, file_path);
        }
        let ok = OpenOptions::new()
            .write(true)
            .create(is_mock)
            .truncate(true)
            .open(path)
            .and_then(|mut file| writeln!(file, "{val}"))
            .is_ok();
        let proc_prefix = format!(
            "{}/",
            self.env.proc_dir.to_string_lossy().trim_end_matches('/')
        );
        let path_s = path.to_string_lossy();
        let short = path_s.strip_prefix(&proc_prefix).unwrap_or(&path_s);
        let rc = i32::from(!ok);
        log::dlog(
            self.env,
            self.cfg,
            "sysctl",
            &format!("rc={rc} {short} = {val}"),
        );
        ok
    }

    /// 与 [`Self::sysw`] 相同，只关心成败：写入失败时计数（调试日志已由 `sysw` 写过），不返回结果。
    pub(crate) fn sysw_run(&self, path: &Path, val: &str) {
        if !self.sysw(path, val) {
            log::note_failure(&log::FAILURES.fs);
        }
    }

    /// 本地网络路由表名：`net.mode` 为 `ndc` 时用系统的 `local_network`，否则用自建的 `97` 号表。
    #[must_use]
    pub(crate) fn lnt(&self) -> &'static str {
        let mode = fs::read_to_string(self.env.run_dir().join("net.mode")).unwrap_or_default();
        if mode.trim() == "ndc" {
            "local_network"
        } else {
            "97"
        }
    }
}

/// 调试开关打开时在后台启动 `xr internal diag-snap <reason>` 抓一次网络快照，不等待。
pub(crate) fn trigger_diag_snap_bg(env: &Env, cfg: &Config, reason: &str) {
    if !cfg.debug_on() {
        return;
    }
    let exe = env.self_exe();
    let child = Command::new(exe)
        .args(["internal", "diag-snap", reason])
        .env("MODDIR", &env.moddir)
        .env("XR_DATA", &env.xr_data)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    crate::sys::exec::note_spawn(env, "diag-snap", child);
}

/// 一个方向对的流量计数（字节）。
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub(crate) struct TrafficCounters {
    /// 上行字节数。
    pub(crate) up: u64,
    /// 下行字节数。
    pub(crate) down: u64,
}

/// 累计流量汇总（`traffic-json` 输出）。
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct TrafficSummary {
    /// 经上游出网的流量。
    pub(crate) wan: TrafficCounters,
    /// 热点等局域网客户端之间的流量。
    pub(crate) lan: TrafficCounters,
    /// 两者合计。
    pub(crate) total: TrafficCounters,
    /// 累计起始时间（Unix 秒）。
    pub(crate) since: u64,
    /// `IPv4` 计数规则是否在位。
    pub(crate) counting: bool,
    /// `IPv6` 计数规则是否在位。
    pub(crate) ipv6: bool,
}

/// 读取流量四元组文件（首行 4 个十进制数：上游上行、上游下行、局域网上行、局域网下行）；缺失或非数字的项为 0。
#[must_use]
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
        if let Some(tok) = parts.next()
            && tok.bytes().all(|byte| byte.is_ascii_digit())
        {
            *slot = tok.parse::<u64>().unwrap_or(0);
        }
    }
    vals
}

/// 原子写入流量四元组：先写 `<文件>.tmp.<pid>` 再改名，写失败时删除临时文件。
pub fn write_quad(env: &Env, path: &Path, vals: [u64; 4]) {
    if let Some(parent) = path.parent() {
        fsx::mkdirs_logged(env, parent);
    }
    let pid = std::process::id();
    let tmp = path.with_extension(format!("tmp.{pid}"));
    let line = format!("{} {} {} {}\n", vals[0], vals[1], vals[2], vals[3]);
    if fs::write(&tmp, line).is_ok() {
        fsx::rename_logged(env, &tmp, path);
    } else {
        fsx::remove_logged(env, &tmp);
    }
}

/// 两个四元组逐项饱和相加。
#[must_use]
pub(crate) const fn combine_quad_add(lhs: [u64; 4], rhs: [u64; 4]) -> [u64; 4] {
    [
        lhs[0].saturating_add(rhs[0]),
        lhs[1].saturating_add(rhs[1]),
        lhs[2].saturating_add(rhs[2]),
        lhs[3].saturating_add(rhs[3]),
    ]
}

/// 两个四元组逐项取较大值。
#[must_use]
pub fn combine_quad_max(lhs: [u64; 4], rhs: [u64; 4]) -> [u64; 4] {
    [
        lhs[0].max(rhs[0]),
        lhs[1].max(rhs[1]),
        lhs[2].max(rhs[2]),
        lhs[3].max(rhs[3]),
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
