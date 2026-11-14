//! 状态页「负载」「设备」磁贴：按需请客机 `xrouter-stats` 采集 `OpenWrt` 负载与在线设备数。
//!
//! 每次读取都为下一轮发出新请求（写 `host/stats.cmd`），已有数据足够新时直接返回，`status-json` 绝大多数情况不等待客机。

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use crate::config::Config;
use crate::env::Env;
use crate::log::unix_now;
use crate::net::common::NetCtx;
use crate::sys::fsx;

#[cfg(test)]
mod tests;

/// 已有数据不超过这个年龄时直接返回、不等待。
const FRESH_MAX: Duration = Duration::from_secs(15);
/// 状态页在没有新鲜数据时最多等待客机应答的时长。
const WAIT_MAX: Duration = Duration::from_millis(2_500);
/// 等待期间的轮询间隔。
const POLL: Duration = Duration::from_millis(100);

/// `OpenWrt` 负载与设备快照。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// CPU 占用（%）。
    pub(crate) cpu_pct: Option<u32>,
    /// 内存占用（%）。
    pub(crate) mem_pct: Option<u32>,
    /// 1 分钟平均负载（原样字符串）。
    pub(crate) load1: Option<String>,
    /// 热点在线设备数。
    pub(crate) hotspot: u32,
    /// 热点是否由 `OpenWrt` 接管（`on`）。
    pub(crate) hs_on: bool,
}

/// 解析后的回执：回显的请求 ts 与快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reply {
    /// 首行回显的请求 ts。
    pub(crate) ts: String,
    /// 快照。
    pub(crate) snap: Snapshot,
}

/// 非负整数字段：不是纯数字时为 `None`（不做浮点运算）。
fn number(value: &str) -> Option<u32> {
    value.parse::<u32>().ok()
}

/// 平均负载字段：只接受非空的数字与小数点组成的串，原样保留。
fn load_text(value: &str) -> Option<String> {
    let valid = !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit() || ch == '.');
    valid.then(|| value.to_owned())
}

/// 解析 `guest/stats`：首行须为 `ts=<请求 ts>`，其后每行一个 `key=value`；首行缺失、为空或不等于 `expect_ts`（若给出）时返回 `None`，值不合法的字段忽略。
#[must_use]
pub(crate) fn parse(body: &str, expect_ts: Option<&str>) -> Option<Reply> {
    let mut lines = body.lines();
    let ts = lines.next()?.strip_prefix("ts=")?.trim();
    if ts.is_empty() || expect_ts.is_some_and(|want| want != ts) {
        return None;
    }
    let mut snap = Snapshot::default();
    for line in lines {
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let value = raw.trim();
        match key.trim() {
            "cpu_pct" => snap.cpu_pct = number(value).or(snap.cpu_pct),
            "mem_pct" => snap.mem_pct = number(value).or(snap.mem_pct),
            "load1" => snap.load1 = load_text(value).or(snap.load1),
            "dev_hotspot" => snap.hotspot = number(value).unwrap_or(snap.hotspot),
            "hs" => snap.hs_on = value == "on",
            _ => {}
        }
    }
    Some(Reply {
        ts: ts.to_owned(),
        snap,
    })
}

/// 统计 `ip neigh` 输出中 `REACHABLE`/`STALE`/`DELAY`/`PROBE` 状态的不同 MAC 数。
#[must_use]
pub(crate) fn count_neigh(text: &str) -> u32 {
    let macs: BTreeSet<&str> = text
        .lines()
        .filter(|line| {
            ["REACHABLE", "STALE", "DELAY", "PROBE"]
                .iter()
                .any(|state| line.contains(state))
        })
        .filter_map(|line| {
            line.split_whitespace()
                .skip_while(|word| *word != "lladdr")
                .nth(1)
        })
        .collect();
    u32::try_from(macs.len()).unwrap_or(u32::MAX)
}

/// 发出一次采集请求：把当前时间与进程号拼成 ts 写入 `host/stats.cmd`，返回该 ts。
pub(crate) fn request(env: &Env) -> String {
    let host = env.run_dir().join("guest/host");
    fsx::mkdirs_logged(env, &host);
    let ts = format!("{}{}", unix_now(), std::process::id());
    fsx::write_logged(env, host.join("stats.cmd"), format!("{ts}\n"));
    ts
}

/// 读取现有回执及其年龄（按文件修改时间；修改时间在未来时按 0 计）。
fn current(path: &Path) -> Option<(Reply, Duration)> {
    let body = fs::read_to_string(path).ok()?;
    let reply = parse(&body, None)?;
    let modified = fs::metadata(path).and_then(|meta| meta.modified()).ok()?;
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    Some((reply, age))
}

/// 等回显 `ts` 的回执，最多 `wait`，每 [`POLL`] 读一次。
fn wait_reply(path: &Path, ts: &str, wait: Duration) -> Option<Reply> {
    let start = Instant::now();
    loop {
        let reply = fs::read_to_string(path)
            .ok()
            .and_then(|body| parse(&body, Some(ts)));
        if reply.is_some() {
            return reply;
        }
        let left = wait.saturating_sub(start.elapsed());
        if left.is_zero() {
            return None;
        }
        thread::sleep(POLL.min(left));
    }
}

/// 设备数：手机固定计 1；热点由 `OpenWrt` 接管时取客机计数，否则手机开着热点时数手机侧邻居表，其余为 0。
fn devices(env: &Env, snap: &Snapshot, hs_if: &str) -> Value {
    let (hotspot, source) = if snap.hs_on {
        (snap.hotspot, "openwrt")
    } else if hs_if.is_empty() {
        (0_u32, "none")
    } else {
        let cfg = Config::load(env);
        let out = NetCtx::new(env, &cfg).ip(&["neigh", "show", "dev", hs_if]);
        (count_neigh(&out.stdout), "phone")
    };
    json!({
        "total": hotspot.saturating_add(1),
        "phone": 1_u32,
        "hotspot": hotspot,
        "source": source,
    })
}

/// 成功应答。
fn ok_value(env: &Env, snap: &Snapshot, age: Duration, hs_if: &str) -> Value {
    json!({
        "ok": true,
        "age_s": age.as_secs(),
        "cpu_pct": snap.cpu_pct,
        "mem_pct": snap.mem_pct,
        "load1": snap.load1,
        "devices": devices(env, snap, hs_if),
    })
}

/// 失败应答。
fn fail_value(reason: &str) -> Value {
    json!({"ok": false, "reason": reason})
}

/// 公共流程：未运行、旧系统盘直接返回；否则发出下一次请求，`fresh` 内的现有数据直接返回，不然最多等 `wait` 拿本次请求的回执。
fn collect(env: &Env, alive: bool, hs_if: &str, fresh: Option<Duration>, wait: Duration) -> Value {
    if !alive {
        return fail_value("stopped");
    }
    let guest = env.run_dir().join("guest/guest");
    if !guest.join("stats.cap").is_file() {
        return fail_value("unsupported");
    }
    let stats = guest.join("stats");
    let existing = current(&stats);
    let ts = request(env);
    if let Some(max) = fresh
        && let Some((reply, age)) = existing.filter(|(_, age)| *age <= max)
    {
        return ok_value(env, &reply.snap, age, hs_if);
    }
    wait_reply(&stats, &ts, wait).map_or_else(
        || fail_value("timeout"),
        |reply| ok_value(env, &reply.snap, Duration::ZERO, hs_if),
    )
}

/// `status-json` 的 `guest_stats` 段：`alive` 为虚拟机是否运行，`hs_if` 为手机侧被接管的热点接口名（无则空串）。
#[must_use]
pub(crate) fn section(env: &Env, alive: bool, hs_if: &str) -> Value {
    collect(env, alive, hs_if, Some(FRESH_MAX), WAIT_MAX)
}

/// `xr guest-stats [wait]`：发一次请求，最多等 `wait_secs` 秒拿本次回执（不复用旧数据）；成功返回 0，否则返回 1。
#[must_use]
pub(crate) fn probe(env: &Env, alive: bool, hs_if: &str, wait_secs: u64) -> (i32, Value) {
    let value = collect(env, alive, hs_if, None, Duration::from_secs(wait_secs));
    let rc = i32::from(value.get("ok").and_then(Value::as_bool) != Some(true));
    (rc, value)
}
