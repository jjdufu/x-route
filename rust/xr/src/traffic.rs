//! 累计流量：在 `xr0` 的 INPUT/OUTPUT 上挂 `XR_ACCT*` 计数链，区分出网与局域网流量，并把计数折算进持久化的基线与快照文件。
//!
//! 基线（`traffic.base`）记录规则重建前已累计的量，快照（`traffic.snap`）在计数规则不在位时提供最后读数。

use std::collections::BTreeSet;
use std::fs;

use crate::env::XR_IF;
use crate::log::unix_now;
use crate::net::common::{
    NetCtx, TrafficCounters, TrafficSummary, combine_quad_add, combine_quad_max, read_quad,
    write_quad,
};
use crate::sys::fsx;
use crate::sys::{LockWait, PathLock};

#[cfg(test)]
mod tests;

/// 获取累计流量的进程间锁 `run/acct.lock`（最多等待约 3 秒）。
fn acct_lock(ctx: &NetCtx<'_>) -> PathLock {
    PathLock::acquire(
        ctx.env,
        ctx.env.run_dir().join("acct.lock"),
        LockWait {
            max_iters: 30,
            sleep_ms: 100,
            check_pid: false,
        },
    )
}

/// `run/routes.active` 中按地址族筛出的非空行（`want_v6` 为真时取含 `:` 的行）。
fn active_routes(ctx: &NetCtx<'_>, want_v6: bool) -> Vec<String> {
    let Ok(active) = fs::read_to_string(ctx.env.run_dir().join("routes.active")) else {
        return Vec::new();
    };
    active
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.contains(':') == want_v6)
        .map(str::to_owned)
        .collect()
}

/// 视为局域网的 `IPv4` 目的网段：私有、CGNAT、链路本地、回环、组播、广播，加上已安装的 `VPN` 路由。
pub fn acct_lan4(ctx: &NetCtx<'_>) -> Vec<String> {
    let mut set: BTreeSet<String> = [
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "100.64.0.0/10",
        "169.254.0.0/16",
        "127.0.0.0/8",
        "224.0.0.0/4",
        "255.255.255.255/32",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let pfx = ctx.cfg.net_prefix();
    set.extend((!pfx.is_empty()).then(|| format!("{pfx}.0/24")));
    set.extend(active_routes(ctx, false));
    set.into_iter().collect()
}

/// 视为局域网的 `IPv6` 网段：ULA、链路本地、组播、回环，加上已安装的 `IPv6` `VPN` 路由。
pub fn acct_lan6(ctx: &NetCtx<'_>) -> Vec<String> {
    let mut set: BTreeSet<String> = ["fc00::/7", "fe80::/10", "ff00::/8", "::1/128"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    set.extend(active_routes(ctx, true));
    set.into_iter().collect()
}

/// 读取一条计数链的字节数：最后一条规则（兜底 `RETURN`）是出网，前面各条之和是局域网，返回 `(局域网, 出网)`。
fn acct_chain_bytes(ctx: &NetCtx<'_>, use_v6: bool, chain: &str) -> (u64, u64) {
    let out = if use_v6 {
        ctx.ip6t(&["-nvxL", chain])
    } else {
        ctx.ipt(&["-nvxL", chain])
    };
    let mut bytes_vec = Vec::new();
    for line in out.stdout.lines().skip(2) {
        if let Some(tok) = line.split_whitespace().nth(1)
            && tok.bytes().all(|byte| byte.is_ascii_digit())
        {
            bytes_vec.push(tok.parse::<u64>().unwrap_or(0));
        }
    }
    let Some((&wan, lan_slice)) = bytes_vec.split_last() else {
        return (0, 0);
    };
    let mut lan = 0_u64;
    for &lan_bytes in lan_slice {
        lan = lan.saturating_add(lan_bytes);
    }
    (lan, wan)
}

/// `IPv4` 计数链 `XR_ACCT_UP` 存在时返回 true。
#[must_use]
pub(crate) fn acct_present(ctx: &NetCtx<'_>) -> bool {
    ctx.ipt(&["-S", "XR_ACCT_UP"]).success()
}

/// `IPv6` 计数链 `XR_ACCT6_UP` 存在时返回 true。
#[must_use]
pub(crate) fn acct_present6(ctx: &NetCtx<'_>) -> bool {
    ctx.ip6t(&["-S", "XR_ACCT6_UP"]).success()
}

/// 计数链当前读数（`IPv4` 与 `IPv6` 相加），顺序为 `[出网上行, 出网下行, 局域网上行, 局域网下行]`。
#[must_use]
pub(crate) fn acct_live(ctx: &NetCtx<'_>) -> [u64; 4] {
    let (up_lan, up_inet) = acct_chain_bytes(ctx, false, "XR_ACCT_UP");
    let (down_lan, down_inet) = acct_chain_bytes(ctx, false, "XR_ACCT_DN");
    let (up_lan_v6, up_inet_v6, down_lan_v6, down_inet_v6) = if acct_present6(ctx) {
        let (ul, uw) = acct_chain_bytes(ctx, true, "XR_ACCT6_UP");
        let (dl, dw) = acct_chain_bytes(ctx, true, "XR_ACCT6_DN");
        (ul, uw, dl, dw)
    } else {
        (0, 0, 0, 0)
    };
    [
        up_inet.saturating_add(up_inet_v6),
        down_inet.saturating_add(down_inet_v6),
        up_lan.saturating_add(up_lan_v6),
        down_lan.saturating_add(down_lan_v6),
    ]
}

/// 累计总量：计数规则在位时为基线加当前读数，否则取基线与快照中的较大值。
#[must_use]
pub(crate) fn acct_total(ctx: &NetCtx<'_>) -> [u64; 4] {
    let base = read_quad(&ctx.env.acct_base_file());
    if acct_present(ctx) {
        combine_quad_add(base, acct_live(ctx))
    } else {
        combine_quad_max(base, read_quad(&ctx.env.acct_snap_file()))
    }
}

/// 在 `acct.lock` 下读取累计总量，避免与 `acct_fold`/`acct_setup` 交错时重复计入。
#[must_use]
pub(crate) fn acct_total_locked(ctx: &NetCtx<'_>) -> [u64; 4] {
    let _held = acct_lock(ctx);
    acct_total(ctx)
}

/// 把当前读数折算进基线（并同步写快照），用于规则重建或拆除之前。
fn acct_fold(ctx: &NetCtx<'_>) {
    let total = combine_quad_add(read_quad(&ctx.env.acct_base_file()), acct_live(ctx));
    write_quad(ctx.env, &ctx.env.acct_base_file(), total);
    write_quad(ctx.env, &ctx.env.acct_snap_file(), total);
}

/// 按局域网网段列表重建计数链 `<prefix>_UP`/`<prefix>_DN` 的规则：每个局域网网段一条 `RETURN`，最后一条兜底 `RETURN`。
fn acct_rules(ctx: &NetCtx<'_>, use_v6: bool, prefix: &str, lan_list: &[String]) {
    let up = format!("{prefix}_UP");
    let dn = format!("{prefix}_DN");
    if use_v6 {
        ctx.ip6t_run(&["-F", &up]);
        ctx.ip6t_run(&["-F", &dn]);
        for lan_net in lan_list {
            ctx.ip6t_run(&["-A", &up, "-d", lan_net, "-j", "RETURN"]);
            ctx.ip6t_run(&["-A", &dn, "-s", lan_net, "-j", "RETURN"]);
        }
        ctx.ip6t_run(&["-A", &up, "-j", "RETURN"]);
        ctx.ip6t_run(&["-A", &dn, "-j", "RETURN"]);
    } else {
        ctx.ipt_run(&["-F", &up]);
        ctx.ipt_run(&["-F", &dn]);
        for net in lan_list {
            ctx.ipt_run(&["-A", &up, "-d", net, "-j", "RETURN"]);
            ctx.ipt_run(&["-A", &dn, "-s", net, "-j", "RETURN"]);
        }
        ctx.ipt_run(&["-A", &up, "-j", "RETURN"]);
        ctx.ipt_run(&["-A", &dn, "-j", "RETURN"]);
    }
}

/// 新建计数链 `<prefix>_UP`/`<prefix>_DN`；链已存在时返回 false。
fn acct_new(ctx: &NetCtx<'_>, use_v6: bool, prefix: &str) -> bool {
    let up = format!("{prefix}_UP");
    let dn = format!("{prefix}_DN");
    let ok = if use_v6 {
        ctx.ip6t(&["-N", &up]).success()
    } else {
        ctx.ipt(&["-N", &up]).success()
    };
    if !ok {
        return false;
    }
    if use_v6 {
        ctx.ip6t_run(&["-N", &dn]);
        ctx.ip6t_run(&["-N", prefix]);
        ctx.ip6t_run(&["-F", prefix]);
        ctx.ip6t_run(&["-A", prefix, "-i", XR_IF, "-j", &up]);
        ctx.ip6t_run(&["-A", prefix, "-o", XR_IF, "-j", &dn]);
    } else {
        ctx.ipt_run(&["-N", &dn]);
        ctx.ipt_run(&["-N", prefix]);
        ctx.ipt_run(&["-F", prefix]);
        ctx.ipt_run(&["-A", prefix, "-i", XR_IF, "-j", &up]);
        ctx.ipt_run(&["-A", prefix, "-o", XR_IF, "-j", &dn]);
    }
    true
}

/// 确保 `INPUT -i xr0`、`OUTPUT -o xr0` 的第一条跳到计数链 `prefix`。
fn acct_hooks_one(ctx: &NetCtx<'_>, use_v6: bool, prefix: &str) {
    if use_v6 {
        if !ctx
            .ip6t(&["-C", "INPUT", "-i", XR_IF, "-j", prefix])
            .success()
        {
            ctx.ip6t_run(&["-I", "INPUT", "1", "-i", XR_IF, "-j", prefix]);
        }
        if !ctx
            .ip6t(&["-C", "OUTPUT", "-o", XR_IF, "-j", prefix])
            .success()
        {
            ctx.ip6t_run(&["-I", "OUTPUT", "1", "-o", XR_IF, "-j", prefix]);
        }
    } else {
        if !ctx
            .ipt(&["-C", "INPUT", "-i", XR_IF, "-j", prefix])
            .success()
        {
            ctx.ipt_run(&["-I", "INPUT", "1", "-i", XR_IF, "-j", prefix]);
        }
        if !ctx
            .ipt(&["-C", "OUTPUT", "-o", XR_IF, "-j", prefix])
            .success()
        {
            ctx.ipt_run(&["-I", "OUTPUT", "1", "-o", XR_IF, "-j", prefix]);
        }
    }
}

/// 为在位的 `IPv4`、`IPv6` 计数链补齐 INPUT/OUTPUT 跳转。
pub(crate) fn acct_hooks(ctx: &NetCtx<'_>) {
    if acct_present(ctx) {
        acct_hooks_one(ctx, false, "XR_ACCT");
    }
    if acct_present6(ctx) {
        acct_hooks_one(ctx, true, "XR_ACCT6");
    }
}

/// 建立或刷新计数规则：首次建链时合并基线与快照并记录起始时间；局域网网段变化时先折算再重建规则，最后补齐跳转。
pub(crate) fn acct_setup(ctx: &NetCtx<'_>, with_v6: bool) {
    let l4 = acct_lan4(ctx);
    let l6 = if with_v6 { acct_lan6(ctx) } else { Vec::new() };
    let sig = format!("4:{}|6:{}", l4.join(" "), l6.join(" "));
    let lock = acct_lock(ctx);
    let fresh = acct_new(ctx, false, "XR_ACCT");
    let fresh6 = with_v6 && acct_new(ctx, true, "XR_ACCT6");
    if fresh {
        let merged = combine_quad_max(
            read_quad(&ctx.env.acct_base_file()),
            read_quad(&ctx.env.acct_snap_file()),
        );
        write_quad(ctx.env, &ctx.env.acct_base_file(), merged);
        let since = ctx.env.acct_since_file();
        if !since.is_file() {
            fsx::write_logged(ctx.env, &since, format!("{}\n", unix_now()).as_bytes());
        }
    }
    let sig_file = ctx.env.run_dir().join("acct.sig");
    let old_sig = fs::read_to_string(&sig_file).unwrap_or_default();
    if fresh || fresh6 || old_sig.trim() != sig.trim() {
        if !fresh {
            acct_fold(ctx);
        }
        acct_rules(ctx, false, "XR_ACCT", &l4);
        if with_v6 {
            acct_rules(ctx, true, "XR_ACCT6", &l6);
        }
        fsx::write_logged(ctx.env, &sig_file, format!("{sig}\n").as_bytes());
    }
    drop(lock);
    acct_hooks(ctx);
}

/// 删除计数链 `prefix` 的 INPUT/OUTPUT 跳转并清空、删除两条计数链。
fn acct_drop(ctx: &NetCtx<'_>, use_v6: bool, prefix: &str) {
    let up = format!("{prefix}_UP");
    let dn = format!("{prefix}_DN");
    if use_v6 {
        while ctx
            .ip6t(&["-D", "INPUT", "-i", XR_IF, "-j", prefix])
            .success()
        {}
        while ctx
            .ip6t(&["-D", "OUTPUT", "-o", XR_IF, "-j", prefix])
            .success()
        {}
        for flush6 in [prefix, &up, &dn] {
            ctx.ip6t_run(&["-F", flush6]);
        }
        for drop6 in [prefix, &up, &dn] {
            ctx.ip6t_run(&["-X", drop6]);
        }
    } else {
        while ctx
            .ipt(&["-D", "INPUT", "-i", XR_IF, "-j", prefix])
            .success()
        {}
        while ctx
            .ipt(&["-D", "OUTPUT", "-o", XR_IF, "-j", prefix])
            .success()
        {}
        for flush4 in [prefix, &up, &dn] {
            ctx.ipt_run(&["-F", flush4]);
        }
        for chain in [prefix, &up, &dn] {
            ctx.ipt_run(&["-X", chain]);
        }
    }
}

/// 拆除计数规则：先把当前读数折算进基线，再删除 `IPv4`、`IPv6` 计数链与 `run/acct.sig`。
pub(crate) fn acct_teardown(ctx: &NetCtx<'_>) {
    let _lock = acct_lock(ctx);
    if acct_present(ctx) {
        acct_fold(ctx);
    }
    acct_drop(ctx, false, "XR_ACCT");
    acct_drop(ctx, true, "XR_ACCT6");
    fsx::remove_logged(ctx.env, ctx.env.run_dir().join("acct.sig"));
}

/// 计数规则在位且总量变化时更新快照文件。
pub(crate) fn acct_snapshot(ctx: &NetCtx<'_>) {
    if !acct_present(ctx) {
        return;
    }
    let _lock = acct_lock(ctx);
    let total = acct_total(ctx);
    let snap_file = ctx.env.acct_snap_file();
    if total != read_quad(&snap_file) {
        write_quad(ctx.env, &snap_file, total);
    }
}

/// 流量快照落盘策略：累计变化达到字节阈值或距上次落盘超过最长间隔才写。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapPolicy {
    /// 四项计数之和相对上次落盘的增量阈值（字节）。
    pub(crate) min_bytes: u64,
    /// 有变化时的最长落盘间隔（秒）。
    pub(crate) max_age: u64,
}

/// 默认：64 MiB 或 30 分钟。掉电或内核崩溃时最多丢失这么多计数。
pub(crate) const SNAP_DEFAULT: SnapPolicy = SnapPolicy {
    min_bytes: 64 * 1024 * 1024,
    max_age: 1_800,
};

impl SnapPolicy {
    /// 取 `Env` 中的阈值（默认 [`SNAP_DEFAULT`]，测试可用环境变量覆盖）。
    #[must_use]
    pub(crate) const fn from_env(env: &crate::env::Env) -> Self {
        Self {
            min_bytes: env.snap_min_bytes,
            max_age: env.snap_max_age,
        }
    }
}

/// 上次落盘的内容与时间（守护进程内存）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapMem {
    /// 上次写入（或启动时读到）的计数。
    pub(crate) quad: [u64; 4],
    /// 上次写入时刻（unix 秒）；0 表示尚未初始化。
    pub(crate) at: u64,
}

/// 两组计数逐项差的绝对值之和（饱和相加）。
#[must_use]
pub(crate) fn sum_delta(left: [u64; 4], right: [u64; 4]) -> u64 {
    left.iter()
        .zip(right.iter())
        .fold(0_u64, |acc, (lhs, rhs)| {
            acc.saturating_add(lhs.abs_diff(*rhs))
        })
}

/// 是否该落盘：没变化不写；未初始化写；增量达到阈值或距上次写入达到最长间隔写；其余不写。
#[must_use]
pub(crate) fn snap_due(total: [u64; 4], mem: &SnapMem, policy: SnapPolicy, now: u64) -> bool {
    if total == mem.quad {
        return false;
    }
    mem.at == 0
        || sum_delta(total, mem.quad) >= policy.min_bytes
        || now.saturating_sub(mem.at) >= policy.max_age
}

/// 守护进程周期性快照：计数规则在位时按 `policy` 决定是否写 `traffic.snap`；返回是否写入。
///
/// 首次调用时用快照文件现有内容初始化 `mem`（时间记为 `now`），不会立刻多写一次；文件内容已等于当前总量
/// （例如刚被折算或命令行快照写过）时只更新 `mem`。
pub(crate) fn acct_snapshot_if(
    ctx: &NetCtx<'_>,
    policy: SnapPolicy,
    mem: &mut SnapMem,
    now: u64,
) -> bool {
    if !acct_present(ctx) {
        return false;
    }
    let _lock = acct_lock(ctx);
    let snap_file = ctx.env.acct_snap_file();
    let on_disk = read_quad(&snap_file);
    if mem.at == 0 {
        *mem = SnapMem {
            quad: on_disk,
            at: now,
        };
    }
    let total = acct_total(ctx);
    if total == on_disk {
        mem.quad = total;
        return false;
    }
    if !snap_due(total, mem, policy, now) {
        return false;
    }
    write_quad(ctx.env, &snap_file, total);
    *mem = SnapMem {
        quad: total,
        at: now,
    };
    true
}

/// 累计流量清零：计数器归零，基线与快照写 0，起始时间改为现在。
pub(crate) fn acct_reset(ctx: &NetCtx<'_>) {
    let _lock = acct_lock(ctx);
    if acct_present(ctx) {
        ctx.ipt_run(&["-Z", "XR_ACCT_UP"]);
        ctx.ipt_run(&["-Z", "XR_ACCT_DN"]);
    }
    if acct_present6(ctx) {
        ctx.ip6t_run(&["-Z", "XR_ACCT6_UP"]);
        ctx.ip6t_run(&["-Z", "XR_ACCT6_DN"]);
    }
    write_quad(ctx.env, &ctx.env.acct_base_file(), [0; 4]);
    write_quad(ctx.env, &ctx.env.acct_snap_file(), [0; 4]);
    fsx::write_logged(
        ctx.env,
        ctx.env.acct_since_file(),
        format!("{}\n", unix_now()).as_bytes(),
    );
}

/// 累计流量汇总（出网、局域网、合计、起始时间与规则在位状态）。
#[must_use]
pub(crate) fn acct_summary(ctx: &NetCtx<'_>) -> TrafficSummary {
    let [wu, wd, lu, ld] = acct_total(ctx);
    let since = fs::read_to_string(ctx.env.acct_since_file())
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0);
    TrafficSummary {
        wan: TrafficCounters { up: wu, down: wd },
        lan: TrafficCounters { up: lu, down: ld },
        total: TrafficCounters {
            up: wu.saturating_add(lu),
            down: wd.saturating_add(ld),
        },
        since,
        counting: acct_present(ctx),
        ipv6: acct_present6(ctx),
    }
}

/// 累计流量汇总的 JSON 文本（`xr traffic-json`）；序列化失败时返回 `{}`。
#[must_use]
pub(crate) fn acct_json(ctx: &NetCtx<'_>) -> String {
    serde_json::to_string(&acct_summary(ctx)).unwrap_or_else(|_| "{}".to_owned())
}
