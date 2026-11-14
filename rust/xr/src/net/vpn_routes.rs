//! 客机 `OpenVPN` 推送的路由：读取、校验（格式、主机位、与现有网段冲突、最多 64 条）并在手机侧经 `xr0` 安装或撤销。

use std::collections::BTreeSet;
use std::fs;
use std::iter;

use crate::cidr;
use crate::cidr::norm_route;
use crate::env::{XR_IF, XR_NET6};
use crate::log;
use crate::log::dlog;
use crate::net::common::NetCtx;
use crate::sys::fsx;

#[cfg(test)]
mod tests;

/// 客机上报 `OpenVPN` 路由的文件 `run/guest/guest/openvpn.routes`。
#[must_use]
pub fn guest_routes_file(ctx: &NetCtx<'_>) -> std::path::PathBuf {
    ctx.env.run_dir().join("guest/guest/openvpn.routes")
}

/// 路由文件缺失时的退路：从 `vm-ctl.log` 中取最后一次 `XR_VPN_ROUTES` 之后的内容。
#[must_use]
pub(crate) fn scrape_ctl_routes(ctx: &NetCtx<'_>) -> String {
    let Ok(content) = fs::read_to_string(ctx.env.ctl_log()) else {
        return String::new();
    };
    let mut last = String::new();
    for line in content.lines() {
        if let Some((_, rhs)) = line.split_once("XR_VPN_ROUTES") {
            rhs.trim().clone_into(&mut last);
        }
    }
    last
}

/// 客机推送的原始路由列表（按逗号或空白拆分），来源见 [`guest_routes_file`] 与 [`scrape_ctl_routes`]。
pub(crate) fn routes_source(ctx: &NetCtx<'_>) -> Vec<String> {
    let gf = guest_routes_file(ctx);
    let raw = if gf.is_file() {
        fs::read_to_string(&gf).unwrap_or_default()
    } else {
        scrape_ctl_routes(ctx)
    };
    raw.split(|ch: char| ch == ',' || ch.is_whitespace())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 路由等价于默认路由（含 `0/1`+`128/1` 这类拆分写法）时返回 true；这些路由不在手机侧安装。
#[must_use]
pub fn is_default_route(route: &str) -> bool {
    matches!(
        route,
        "default" | "0.0.0.0/0" | "0.0.0.0/1" | "128.0.0.0/1" | "::/0" | "::/1" | "8000::/1"
    )
}

/// 路由含 `:`（即 `IPv6`）时返回 true。
#[must_use]
pub(crate) fn is_ip6_cidr(route: &str) -> bool {
    route.contains(':')
}

/// 共享网络（热点、USB 共享等）接口列表：`local_network` 表中出现的 `dev`，排除 `xr0`。
#[must_use]
pub(crate) fn tether_ifaces(ctx: &NetCtx<'_>) -> Vec<String> {
    let out = ctx.ip(&["route", "show", "table", "local_network"]);
    let mut set = BTreeSet::new();
    for line in out.stdout.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        for (idx, tok) in tokens.iter().enumerate() {
            if *tok == "dev"
                && let Some(dev) = tokens.get(idx.saturating_add(1))
                && *dev != XR_IF
            {
                set.extend(iter::once((*dev).to_owned()));
            }
        }
    }
    set.into_iter().collect()
}

/// `VPN` 路由被拒绝的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RejectKind {
    /// 与当前上游或共享接口网段重叠：本地直连优先（随网络切换自动恢复）。
    Local,
    /// 与 `XRouter` 保留网段（`xr0` 的 /24、回环、链路本地、`XR_NET6`、热点网关）重叠。
    Reserved,
    /// 无法解析。
    Format,
    /// 主机位不为 0。
    HostBits,
    /// 超出 64 条上限。
    Limit,
}

impl RejectKind {
    /// 落盘与状态输出用的短名。
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Reserved => "reserved",
            Self::Format => "format",
            Self::HostBits => "hostbits",
            Self::Limit => "limit",
        }
    }

    /// 由短名解析；未知名字返回 `None`。
    #[must_use]
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "local" => Some(Self::Local),
            "reserved" => Some(Self::Reserved),
            "format" => Some(Self::Format),
            "hostbits" => Some(Self::HostBits),
            "limit" => Some(Self::Limit),
            _ => None,
        }
    }
}

/// 一条被拒绝的路由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rejected {
    /// 规范化后的路由（无法解析时为原文）。
    pub(crate) route: String,
    /// 原因。
    pub(crate) kind: RejectKind,
    /// 与之重叠的网段（`Local`/`Reserved` 时有值，其余为空串）。
    pub(crate) with: String,
}

/// 路由检查结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RouteCheck {
    /// 可下发的路由（有序去重）。
    pub(crate) valid: Vec<String>,
    /// 被拒绝的路由（按客机上报顺序，按 route 去重）。
    pub(crate) rejected: Vec<Rejected>,
}

/// 不能下发的网段，分两类。
#[derive(Debug, Default)]
struct BusyNets {
    /// `XRouter` 保留网段：永远不会放行。
    reserved: Vec<String>,
    /// 当前上游与共享接口的本地网段：随网络切换变化。
    local: Vec<String>,
}

/// 单条路由的判定结果。
enum Verdict {
    /// 默认路由：静默跳过。
    Skip,
    /// 可以下发（规范写法）。
    Valid(String),
    /// 被拒绝。
    Reject(Rejected),
}

/// 构造一条拒绝记录。
fn reject(route: &str, kind: RejectKind, with: &str) -> Verdict {
    Verdict::Reject(Rejected {
        route: route.to_owned(),
        kind,
        with: with.to_owned(),
    })
}

/// 在网段列表中找第一个与 `route` 重叠的项。
fn first_overlap<'list>(route: &str, nets: &'list [String]) -> Option<&'list String> {
    nets.iter()
        .find(|used| !used.is_empty() && cidr::overlap(route, used))
}

/// 单条路由判定：默认路由跳过；依次检查格式、主机位、保留网段、本地网段。
fn classify(raw: &str, busy: &BusyNets) -> Verdict {
    if is_default_route(raw) {
        return Verdict::Skip;
    }
    let Some(nc) = norm_route(raw) else {
        return reject(raw, RejectKind::Format, "");
    };
    if is_default_route(&nc) {
        return Verdict::Skip;
    }
    if !cidr::is_aligned(&nc) {
        return reject(&nc, RejectKind::HostBits, "");
    }
    if let Some(conflict) = first_overlap(&nc, &busy.reserved) {
        return reject(&nc, RejectKind::Reserved, conflict);
    }
    if let Some(conflict) = first_overlap(&nc, &busy.local) {
        return reject(&nc, RejectKind::Local, conflict);
    }
    Verdict::Valid(nc)
}

/// 接口上的 `IPv4` 与 `IPv6` 地址（`ip -o addr show` 第 4 列）。
fn iface_addrs(ctx: &NetCtx<'_>, iface: &str) -> Vec<String> {
    let mut addrs = Vec::new();
    for family in ["-4", "-6"] {
        let out = ctx.ip(&[family, "-o", "addr", "show", "dev", iface]);
        addrs.extend(
            out.stdout
                .lines()
                .filter_map(|line| line.split_whitespace().nth(3))
                .map(str::to_owned),
        );
    }
    addrs
}

/// 收集保留网段与本地网段。
fn busy_nets(ctx: &NetCtx<'_>) -> BusyNets {
    let mut busy = BusyNets {
        reserved: vec![
            format!("{}.0/24", ctx.cfg.net_prefix()),
            "127.0.0.0/8".to_owned(),
            "169.254.0.0/16".to_owned(),
            format!("{XR_NET6}::/64"),
        ],
        local: Vec::new(),
    };
    if let Ok(hs_gw) = fs::read_to_string(ctx.env.run_dir().join("net.hs_gw")) {
        busy.reserved
            .extend(hs_gw.split_whitespace().map(str::to_owned));
    }
    let up = fs::read_to_string(ctx.env.run_dir().join("net.up_if")).unwrap_or_default();
    let mut ifaces = Vec::new();
    if !up.trim().is_empty() {
        ifaces.push(up.trim().to_owned());
    }
    ifaces.extend(tether_ifaces(ctx));
    for iface in ifaces {
        busy.local.extend(iface_addrs(ctx, &iface));
    }
    busy
}

/// 按规范写法去重（无法解析的按原文），保留首次出现的顺序。
fn dedup_routes(raw_list: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    raw_list
        .into_iter()
        .filter(|raw| seen.insert(norm_route(raw).unwrap_or_else(|| raw.clone())))
        .collect()
}

/// 可下发路由的上限。
const ROUTE_LIMIT: usize = 64;

/// 检查客机推送的路由：先按规范写法去重，再逐条判定；可下发的超过 64 条时，第 65 条起都记为 `Limit`。
#[must_use]
pub(crate) fn routes_check(ctx: &NetCtx<'_>) -> RouteCheck {
    let busy = busy_nets(ctx);
    let mut check = RouteCheck::default();
    let mut valid_set = BTreeSet::new();
    let mut pending = dedup_routes(routes_source(ctx)).into_iter();
    for raw in pending.by_ref() {
        match classify(&raw, &busy) {
            Verdict::Skip => {}
            Verdict::Reject(rejected) => check.rejected.push(rejected),
            Verdict::Valid(nc) if valid_set.len() >= ROUTE_LIMIT => {
                check.rejected.push(Rejected {
                    route: nc,
                    kind: RejectKind::Limit,
                    with: String::new(),
                });
                break;
            }
            Verdict::Valid(nc) => valid_set.extend(iter::once(nc)),
        }
    }
    check.rejected.extend(
        pending
            .filter(|raw| !is_default_route(raw))
            .map(|raw| Rejected {
                route: norm_route(&raw).unwrap_or(raw),
                kind: RejectKind::Limit,
                with: String::new(),
            }),
    );
    check.valid = valid_set.into_iter().collect();
    check
}

/// 筛出可以安装的 `VPN` 路由（即 `routes_check` 的 `valid`）：规范化、去重、跳过默认路由，拒绝格式错误、主机位非 0、与现有网段重叠的项，最多 64 条。
#[must_use]
pub fn routes_valid(ctx: &NetCtx<'_>) -> Vec<String> {
    routes_check(ctx).valid
}

/// 拒绝原因文件 `run/routes.rejected`。
fn rejected_file(ctx: &NetCtx<'_>) -> std::path::PathBuf {
    ctx.env.run_dir().join("routes.rejected")
}

/// 拒绝记录落盘内容：每行 `route\tkind\twith`，按 route 排序；没有记录时为空串。
fn rejected_body(rejected: &[Rejected]) -> String {
    let mut lines: Vec<String> = rejected
        .iter()
        .map(|item| format!("{}\t{}\t{}\n", item.route, item.kind.as_str(), item.with))
        .collect();
    lines.sort();
    lines.concat()
}

/// 解析 `run/routes.rejected` 的内容；无法识别的行跳过。
#[must_use]
pub(crate) fn parse_rejected(text: &str) -> Vec<Rejected> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let route = fields.next().filter(|route| !route.is_empty())?;
            let kind = RejectKind::parse(fields.next()?)?;
            Some(Rejected {
                route: route.to_owned(),
                kind,
                with: fields.next().unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

/// 新出现的一条拒绝记录写一行日志：本地直连写 INFO，其余写 ERROR（超限只由调用方写一次）。
fn log_new_reject(ctx: &NetCtx<'_>, item: &Rejected) {
    let route = &item.route;
    let with = &item.with;
    match item.kind {
        RejectKind::Local => log::info(
            ctx.env,
            "vpn",
            &format!(
                "VPN 路由 {route} 与当前网络 {with} 同网段，本地直连优先，暂不经 VPN（切换网络后自动恢复）"
            ),
        ),
        RejectKind::Reserved => log::err(ctx.env, "vpn", &format!("{route} 与 {with} 重叠")),
        RejectKind::Format => log::err(ctx.env, "vpn", &format!("格式错误 {route}")),
        RejectKind::HostBits => log::err(ctx.env, "vpn", &format!("主机位不为 0：{route}")),
        RejectKind::Limit => {}
    }
}

/// 写 `run/routes.rejected`（内容不变不写）；内容变化时只为新出现的条目写日志，本地直连条目消失时写"已恢复经 VPN"。
fn sync_rejected(ctx: &NetCtx<'_>, rejected: &[Rejected]) {
    let path = rejected_file(ctx);
    let old = parse_rejected(&fs::read_to_string(&path).unwrap_or_default());
    if !fsx::write_if_changed_logged(ctx.env, &path, &rejected_body(rejected)) {
        return;
    }
    let is_new = |item: &&Rejected| {
        !old.iter()
            .any(|prev| prev.route == item.route && prev.kind == item.kind)
    };
    let fresh: Vec<&Rejected> = rejected.iter().filter(is_new).collect();
    for item in &fresh {
        log_new_reject(ctx, item);
    }
    if fresh.iter().any(|item| item.kind == RejectKind::Limit) {
        log::err(ctx.env, "vpn", "超过 64 条，其余忽略");
    }
    for prev in old.iter().filter(|prev| prev.kind == RejectKind::Local) {
        if !rejected
            .iter()
            .any(|item| item.route == prev.route && item.kind == RejectKind::Local)
        {
            log::info(
                ctx.env,
                "vpn",
                &format!("VPN 路由 {} 已恢复经 VPN", prev.route),
            );
        }
    }
}

/// 路由下一跳：`IPv6` 为客机的 `<ULA 前缀>::254`，`IPv4` 为 `<内网前缀>.254`。
fn route_via(ctx: &NetCtx<'_>, cidr: &str) -> String {
    if is_ip6_cidr(cidr) {
        format!("{XR_NET6}::254")
    } else {
        format!("{}.254", ctx.cfg.net_prefix())
    }
}

/// 按 `cidr` 的地址族执行 `ip` 或 `ip -6`。
fn ipx(ctx: &NetCtx<'_>, cidr: &str, args: &[&str]) -> crate::sys::exec::Output {
    if is_ip6_cidr(cidr) {
        ctx.ip6(args)
    } else {
        ctx.ip(args)
    }
}

/// 与 [`ipx`] 相同（按 `cidr` 的地址族执行 `ip` 或 `ip -6`），只关心成败：失败时计数，不返回结果。
fn ipx_run(ctx: &NetCtx<'_>, cidr: &str, args: &[&str]) {
    crate::sys::exec::note_exec_result(&ipx(ctx, cidr, args));
}

/// 安装一条 `VPN` 路由：`ndc` 模式下经 `ndc network route add`，否则写入 97 号表；再加一条优先级 4801 的"去往该网段查本地网络表"规则。
pub(crate) fn route_add(ctx: &NetCtx<'_>, cidr: &str) {
    let via = route_via(ctx, cidr);
    let mode = fs::read_to_string(ctx.env.run_dir().join("net.mode")).unwrap_or_default();
    if mode.trim() == "ndc" {
        ctx.ndc_run(&["network", "route", "add", "local", XR_IF, cidr, &via]);
    } else {
        ipx_run(
            ctx,
            cidr,
            &[
                "route", "replace", cidr, "via", &via, "dev", XR_IF, "table", "97",
            ],
        );
    }
    ipx_run(ctx, cidr, &["rule", "del", "pref", "4801", "to", cidr]);
    let tbl = ctx.lnt();
    ipx_run(
        ctx,
        cidr,
        &["rule", "add", "pref", "4801", "to", cidr, "lookup", tbl],
    );
}

/// 撤销一条 `VPN` 路由（`ndc` 与 97 号表两处都删）及其 4801 规则。
pub(crate) fn route_del(ctx: &NetCtx<'_>, cidr: &str) {
    let via = route_via(ctx, cidr);
    ctx.ndc_run(&["network", "route", "remove", "local", XR_IF, cidr, &via]);
    ipx_run(ctx, cidr, &["route", "del", cidr, "table", "97"]);
    ipx_run(ctx, cidr, &["rule", "del", "pref", "4801", "to", cidr]);
}

/// 字节序列的简单指纹（31 进制累乘 + 长度），用于判断路由文件是否变化。
#[must_use]
pub(crate) fn cksum_crc_like(bytes: &[u8]) -> String {
    // Simple deterministic fingerprint matching cksum presence/absence semantics for net_watch
    let mut crc: u32 = 0;
    for &byte in bytes {
        crc = crc.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    format!("{crc} {}", bytes.len())
}

/// 路由文件的指纹；文件不存在时为 `none`。
#[must_use]
pub(crate) fn routes_file_cksum(ctx: &NetCtx<'_>) -> String {
    fs::read(guest_routes_file(ctx))
        .map_or_else(|_| "none".to_owned(), |bytes| cksum_crc_like(&bytes))
}

/// 让已安装路由与 [`routes_check`] 的结果一致：删多余、补缺失，写 `run/routes.active`、`run/routes.rejected` 与 `run/vpn.cksum`；变化时写日志。
pub(crate) fn routes_sync(ctx: &NetCtx<'_>) {
    let check = routes_check(ctx);
    sync_rejected(ctx, &check.rejected);
    let want = check.valid;
    let active_path = ctx.env.run_dir().join("routes.active");
    let have: Vec<String> = fs::read_to_string(&active_path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .collect();

    if want != have {
        dlog(
            ctx.env,
            ctx.cfg,
            "vpn",
            &format!(
                "OpenVPN 路由变化：[{}] -> [{}]",
                have.join(" "),
                want.join(" ")
            ),
        );
    }
    for route in &have {
        if !want.contains(route) {
            route_del(ctx, route);
        }
    }
    for w in &want {
        if !have.contains(w) {
            route_add(ctx, w);
        }
    }
    let body = if want.is_empty() {
        String::new()
    } else {
        format!("{}\n", want.join("\n"))
    };
    fsx::write_logged(ctx.env, &active_path, body);
    fsx::write_logged(
        ctx.env,
        ctx.env.run_dir().join("vpn.cksum"),
        format!("{}\n", routes_file_cksum(ctx)),
    );
}

/// 补回 `run/routes.active` 中在 97 号表里找不到的路由（被外部删除时）。
pub(crate) fn routes_reassert(ctx: &NetCtx<'_>) {
    let Ok(active) = fs::read_to_string(ctx.env.run_dir().join("routes.active")) else {
        return;
    };
    for route_cidr in active
        .lines()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let out = ipx(ctx, route_cidr, &["route", "show", "table", "97"]);
        let needle = format!("{route_cidr} via");
        if !out.stdout.lines().any(|line| line.starts_with(&needle)) {
            route_add(ctx, route_cidr);
        }
    }
}

/// 撤销 `run/routes.active` 中的全部路由并删除该文件。
pub(crate) fn routes_clear(ctx: &NetCtx<'_>) {
    let active_path = ctx.env.run_dir().join("routes.active");
    if let Ok(active) = fs::read_to_string(&active_path) {
        for route in active
            .lines()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            route_del(ctx, route);
        }
    }
    fsx::remove_logged(ctx.env, &active_path);
    fsx::remove_logged(ctx.env, rejected_file(ctx));
}
