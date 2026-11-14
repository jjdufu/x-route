//! 手机侧网络：`xr0` 的创建与拆除、上游跟随、策略路由与网关模式、转发与 NAT 规则、MTU/MSS，以及 `net status` 状态。
//!
//! 由 `xr net up|down|watch|sync|status` 调用；`watch` 每 10 秒对账一次，`sync` 在网络变化时由守护进程触发。

pub mod common;
pub(crate) mod hotspot;
pub(crate) mod ipv6;
pub mod vpn_routes;

use std::collections::BTreeSet;
use std::fs;
use std::iter;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde::Serialize;

use crate::config::Config;
use crate::env::{Env, HS_BR, HS_TAP, XR_IF, XR_NET6};
use crate::log;
use crate::log::{dlog, unix_now};
use crate::net::common::{NetCtx, trigger_diag_snap_bg};
use crate::net::hotspot::{hs_fwd6_rules, hs_sync, hs_tap_down, hs_tap_up, vm_alive, vm_is_ready};
use crate::net::ipv6::{
    v6_nft_setup, v6_nft_sync, v6_nft_teardown, v6_proxy_remove, v6_proxy_sync, v6_src_fingerprint,
    vm_v6,
};
use crate::net::vpn_routes::{
    routes_clear, routes_file_cksum, routes_reassert, routes_sync, tether_ifaces,
};
use crate::out::emit;
use crate::sys::exec::note_exec_result;
use crate::sys::fsx;
use crate::sys::spawn;
use crate::sys::{LockWait, PathLock, send_signal};
use crate::traffic::{acct_hooks, acct_present, acct_present6, acct_setup, acct_teardown};

/// 网关模式（`global`）下全局路由规则的优先级。
const GW_PREF: &str = "19100";
/// 网关模式使用的路由表号。
const GW_TABLE: &str = "233";
/// `IPv6` 拦截规则（热点等接口的 `IPv6` 指向 `xr0`）的优先级。
const REJ6_PREF: &str = "19060";
/// `IPv6` 拦截使用的路由表号。
const REJ6_TABLE: &str = "235";

/// 接口名属于蜂窝数据网卡（`rmnet`、`ccmni`、`seth` 及其 `v4-` 464XLAT 接口）时返回 true。
fn is_cell_iface(name: &str) -> bool {
    name.starts_with("rmnet")
        || name.starts_with("ccmni")
        || name.starts_with("seth")
        || name.starts_with("v4-rmnet")
        || name.starts_with("v4-ccmni")
}

/// 获取网络操作的进程间锁 `run/.net.lock`（最多等待约 60 秒，持有者进程已退出时接管）。
fn net_lock(env: &Env) -> PathLock {
    PathLock::acquire(
        env,
        env.run_dir().join(".net.lock"),
        LockWait {
            max_iters: 120,
            sleep_ms: 500,
            check_pid: true,
        },
    )
}

/// 当前默认上游接口：取 `ip rule` 中 `fwmark 0x0/0xffff iif lo lookup <接口>` 的表名，找不到时返回空串。
#[must_use]
pub(crate) fn upstream_iface(ctx: &NetCtx<'_>) -> String {
    let out = ctx.ip(&["rule"]);
    for line in out.stdout.lines() {
        if let Some((_, rhs)) = line.split_once("fwmark 0x0/0xffff iif lo lookup ")
            && let Some(iface) = rhs.split_whitespace().next()
        {
            return iface.to_owned();
        }
    }
    String::new()
}

/// 路由表输出中所有 `default` 行里紧跟在 `key` 之后的值，按出现顺序返回。
fn default_route_values<'a>(stdout: &'a str, key: &str) -> Vec<&'a str> {
    let mut values = Vec::new();
    for line in stdout.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first().copied() != Some("default") {
            continue;
        }
        for pair in tokens.windows(2) {
            if let [tok, value] = *pair
                && tok == key
            {
                values.push(value);
            }
        }
    }
    values
}

/// 去掉路由行中的 `expires <秒数>` 一对标记，其余标记按原顺序返回。
fn strip_expires<'a>(tokens: &[&'a str]) -> Vec<&'a str> {
    let mut kept = Vec::with_capacity(tokens.len());
    let mut skip_next = false;
    for &tok in tokens {
        if skip_next {
            skip_next = false;
            continue;
        }
        if tok == "expires" {
            skip_next = true;
            continue;
        }
        kept.push(tok);
    }
    kept
}

/// 上游的 `IPv4` 出口接口集合：上游自身加上其路由表中默认路由的 `dev`，排序去重。
#[must_use]
pub(crate) fn upstream_out(ctx: &NetCtx<'_>, up: &str) -> Vec<String> {
    if up.is_empty() {
        return Vec::new();
    }
    let mut set = BTreeSet::new();
    set.extend(iter::once(up.to_owned()));
    let out = ctx.ip(&["-4", "route", "show", "table", up]);
    for dev in default_route_values(&out.stdout, "dev") {
        set.extend(iter::once(dev.to_owned()));
    }
    set.into_iter().collect()
}

/// 上游的 `IPv6` 出口接口集合：上游自身（`v4-` 接口除外）加上其 `IPv6` 默认路由的 `dev`（同样排除 `v4-`）。
#[must_use]
pub(crate) fn upstream_out6(ctx: &NetCtx<'_>, up: &str) -> Vec<String> {
    if up.is_empty() {
        return Vec::new();
    }
    let mut set = BTreeSet::new();
    if !up.starts_with("v4-") {
        set.extend(iter::once(up.to_owned()));
    }
    let out = ctx.ip6(&["route", "show", "table", up]);
    for dev in default_route_values(&out.stdout, "dev") {
        if !dev.starts_with("v4-") {
            set.extend(iter::once(dev.to_owned()));
        }
    }
    set.into_iter().collect()
}

/// 按 `XR_EXPOSE` 计算需要开放管理访问的接口：`lan` 为共享网络接口，`all` 再加上游；蜂窝网卡始终排除。
#[must_use]
pub(crate) fn expose_ifaces(ctx: &NetCtx<'_>, up: &str) -> Vec<String> {
    let mut set = BTreeSet::new();
    match ctx.cfg.expose() {
        "lan" => {
            for i in tether_ifaces(ctx) {
                set.extend(iter::once(i));
            }
        }
        "all" => {
            for i in tether_ifaces(ctx) {
                set.extend(iter::once(i));
            }
            if !up.is_empty() {
                set.extend(iter::once(up.to_owned()));
            }
        }
        _ => {}
    }
    set.into_iter().filter(|i| !is_cell_iface(i)).collect()
}

/// `xr0` 应使用的 MTU：出口接口 MTU 与上游默认路由 `mtu` 中的最小值，不低于 1280、不高于 1500。
#[must_use]
pub(crate) fn upstream_mtu(ctx: &NetCtx<'_>, up: &str, outs: &[String]) -> u32 {
    let mut min_mtu: u32 = 1500;
    for out_if in outs {
        if let Ok(raw) = fs::read_to_string(ctx.env.sysnet.join(out_if).join("mtu"))
            && let Ok(val) = raw.trim().parse::<u32>()
            && val < min_mtu
        {
            min_mtu = val;
        }
    }
    if !up.is_empty() {
        for out in [
            ctx.ip(&["-4", "route", "show", "table", up]),
            ctx.ip6(&["route", "show", "table", up]),
        ] {
            let route_min = default_route_values(&out.stdout, "mtu")
                .into_iter()
                .filter_map(|text| text.parse::<u32>().ok())
                .min();
            if let Some(value) = route_min
                && value < min_mtu
            {
                min_mtu = value;
            }
        }
    }
    min_mtu.max(1280)
}

/// 在 mangle 表的 `XR_MSS` 链为 `xr0` 和各出口接口设置 TCP MSS 钳制（`IPv4`、`IPv6` 各一份），并确保 `POSTROUTING` 第一条跳到该链。
pub(crate) fn mss_rules(ctx: &NetCtx<'_>, outs: &[String]) {
    let mut targets = vec![XR_IF.to_owned()];
    targets.extend_from_slice(outs);
    for use_v6 in [false, true] {
        let run_t = |args: &[&str]| {
            if use_v6 {
                ctx.ip6t(args)
            } else {
                ctx.ipt(args)
            }
        };
        note_exec_result(&run_t(&["-t", "mangle", "-N", "XR_MSS"]));
        if !run_t(&["-t", "mangle", "-F", "XR_MSS"]).success() {
            continue;
        }
        for out_if in &targets {
            note_exec_result(&run_t(&[
                "-t",
                "mangle",
                "-A",
                "XR_MSS",
                "-o",
                out_if,
                "-p",
                "tcp",
                "--tcp-flags",
                "SYN,RST",
                "SYN",
                "-j",
                "TCPMSS",
                "--clamp-mss-to-pmtu",
            ]));
        }
        if !run_t(&["-t", "mangle", "-C", "POSTROUTING", "-j", "XR_MSS"]).success() {
            note_exec_result(&run_t(&[
                "-t",
                "mangle",
                "-I",
                "POSTROUTING",
                "1",
                "-j",
                "XR_MSS",
            ]));
        }
    }
}

/// 删除 `IPv4`、`IPv6` 的 `XR_MSS` 链及其在 `POSTROUTING` 中的跳转。
pub(crate) fn mss_clear(ctx: &NetCtx<'_>) {
    for use_v6 in [false, true] {
        let run_t = |args: &[&str]| {
            if use_v6 {
                ctx.ip6t(args)
            } else {
                ctx.ipt(args)
            }
        };
        while run_t(&["-t", "mangle", "-D", "POSTROUTING", "-j", "XR_MSS"]).success() {}
        note_exec_result(&run_t(&["-t", "mangle", "-F", "XR_MSS"]));
        note_exec_result(&run_t(&["-t", "mangle", "-X", "XR_MSS"]));
    }
}

/// 把 `xr0` 的 MTU 调到跟随上游的值（变化时记 INFO），写 `run/net.mtu` 和给客机的 `guest/host/mtu`，并刷新 MSS 钳制。
pub(crate) fn mtu_sync(ctx: &NetCtx<'_>, up: &str, outs: &[String]) {
    let mt = upstream_mtu(ctx, up, outs);
    let mt_s = mt.to_string();
    let cur = fs::read_to_string(ctx.env.sysnet.join(XR_IF).join("mtu"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if cur != mt_s && ctx.ip(&["link", "set", XR_IF, "mtu", &mt_s]).success() {
        let cur_disp = if cur.is_empty() { "?" } else { &cur };
        let up_disp = if up.is_empty() { "无" } else { up };
        log::info(
            ctx.env,
            "net",
            &format!("xr0 MTU {cur_disp} -> {mt_s}（跟随上游 {up_disp}）"),
        );
    }
    fsx::write_logged(
        ctx.env,
        ctx.env.run_dir().join("net.mtu"),
        format!("{mt_s}\n"),
    );
    let host_dir = ctx.env.run_dir().join("guest/host");
    fsx::mkdirs_logged(ctx.env, &host_dir);
    let guest_mtu = host_dir.join("mtu");
    if fs::read_to_string(&guest_mtu).map_or(true, |text| text.trim() != mt_s) {
        fsx::write_logged(ctx.env, guest_mtu, format!("{mt_s}\n"));
    }
    mss_rules(ctx, outs);
}

/// 给客机推送当前时间：`guest/host/time` 缺失、损坏、晚于现在或已过 30 分钟时写入当前 Unix 秒。
pub(crate) fn time_push(env: &Env) {
    let host_dir = env.run_dir().join("guest/host");
    if !host_dir.is_dir() {
        return;
    }
    let now = unix_now();
    let time_file = host_dir.join("time");
    let tp = fs::read_to_string(&time_file)
        .ok()
        .and_then(|text| {
            let trimmed = text.trim();
            if trimmed.len() <= 10 && trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
                trimmed.parse::<u64>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    if now >= tp && now.saturating_sub(tp) < 1800 {
        return;
    }
    fsx::write_logged(env, time_file, format!("{now}\n"));
}

/// 确保 `FORWARD` 链第一条是跳到 `XR_FWD`，否则删除所有旧跳转后插到第一位。
fn fwd_first(ctx: &NetCtx<'_>) {
    let out = ctx.ipt(&["-S", "FORWARD"]);
    if out.stdout.lines().nth(1) == Some("-A FORWARD -j XR_FWD") {
        return;
    }
    while ctx.ipt(&["-D", "FORWARD", "-j", "XR_FWD"]).success() {}
    ctx.ipt_run(&["-I", "FORWARD", "1", "-j", "XR_FWD"]);
}

/// 确保 nat 表 `POSTROUTING` 中有跳到 `XR_NAT` 的规则，没有时插到第一位。
fn nat_first(ctx: &NetCtx<'_>) {
    if !ctx
        .ipt(&["-t", "nat", "-C", "POSTROUTING", "-j", "XR_NAT"])
        .success()
    {
        ctx.ipt_run(&["-t", "nat", "-I", "POSTROUTING", "1", "-j", "XR_NAT"]);
    }
}

/// 确保 `IPv6` `FORWARD` 链第一条是跳到 `XR_FWD6`，否则删除所有旧跳转后插到第一位。
fn fwd6_first(ctx: &NetCtx<'_>) {
    let out = ctx.ip6t(&["-S", "FORWARD"]);
    if out.stdout.lines().nth(1) == Some("-A FORWARD -j XR_FWD6") {
        return;
    }
    while ctx.ip6t(&["-D", "FORWARD", "-j", "XR_FWD6"]).success() {}
    ctx.ip6t_run(&["-I", "FORWARD", "1", "-j", "XR_FWD6"]);
}

/// `ip6tables` 的 nat 表是否可用；结果缓存到 `run/net.ip6nat`，之后直接读缓存。
#[must_use]
pub(crate) fn ip6_nat_ok(env: &Env) -> bool {
    let cache = env.run_dir().join("net.ip6nat");
    if let Ok(raw) = fs::read_to_string(&cache) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed == "1";
        }
    }
    let ok = Command::new(env.sysbin_cmd("ip6tables"))
        .args(["-w", "-t", "nat", "-S", "POSTROUTING"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|exit_status| exit_status.success());
    fsx::write_logged(env, cache, if ok { "1\n" } else { "0\n" });
    ok
}

/// `IPv6` nat 可用时删除所有残留的 `POSTROUTING` → `XR_NAT6` 跳转（当前版本不做 `IPv6` NAT）。
fn nat6_first(ctx: &NetCtx<'_>) {
    if !ip6_nat_ok(ctx.env) {
        return;
    }
    while ctx
        .ip6t(&["-t", "nat", "-D", "POSTROUTING", "-j", "XR_NAT6"])
        .success()
    {}
}

/// 是否应启用网关：模式为 `global` 且虚拟机已就绪。
fn want_gateway(ctx: &NetCtx<'_>) -> bool {
    ctx.cfg.mode() == "global" && vm_is_ready(ctx.env)
}

/// 应使用的网关形态：`off`（不接管）、`v6`（虚拟机有 `IPv6` 且邻居代理已安装，`IPv6` 也走虚拟机）或 `block`（`IPv6` 出网被拦截）。
fn gw_want(ctx: &NetCtx<'_>) -> &'static str {
    if !want_gateway(ctx) {
        "off"
    } else if vm_v6(ctx.env) && ctx.env.run_dir().join("net.v6proxy").is_file() {
        "v6"
    } else {
        "block"
    }
}

/// 网关形态变化时写进日志的原因说明：模式、虚拟机状态、客机上报的 `IPv6` 与邻居代理是否安装。
fn gw_reason(ctx: &NetCtx<'_>) -> String {
    let vm_st = if vm_alive(ctx.env) {
        fs::read_to_string(ctx.env.run_dir().join("state")).unwrap_or_else(|_| "booting".to_owned())
    } else {
        "未运行".to_owned()
    };
    let g_v6 = fs::read_to_string(ctx.env.run_dir().join("guest/guest/ipv6"))
        .unwrap_or_else(|_| "未上报".to_owned());
    let nd = if ctx.env.run_dir().join("net.v6proxy").is_file() {
        "已安装"
    } else {
        "未安装"
    };
    format!(
        "模式={} 虚拟机={} 客侧IPv6={} 邻居代理={nd}",
        ctx.cfg.mode(),
        vm_st.trim(),
        g_v6.trim()
    )
}

/// 把接口 `dev` 的非默认 `IPv4` 路由复制到网关路由表，让直连网段不经虚拟机。
fn copy_dev_routes(ctx: &NetCtx<'_>, dev: &str) {
    if dev.is_empty() || dev == XR_IF {
        return;
    }
    let out = ctx.ip(&["-4", "route", "show", "dev", dev]);
    for line in out.stdout.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() || tokens.first().copied() == Some("default") {
            continue;
        }
        let mut args: Vec<&str> = Vec::with_capacity(tokens.len().saturating_add(5));
        args.extend(["route", "replace"]);
        args.extend(tokens);
        args.extend(["dev", dev, "table", GW_TABLE]);
        ctx.ip_run(&args);
    }
}

/// 把接口 `dev` 在 `main`、`local_network` 表中的 `IPv6` 单播路由（去掉 `expires`）复制到网关路由表。
fn copy_dev_routes6(ctx: &NetCtx<'_>, dev: &str) {
    if dev.is_empty() || dev == XR_IF {
        return;
    }
    for tb in ["main", "local_network"] {
        let out = ctx.ip6(&["route", "show", "table", tb, "dev", dev]);
        for line in out.stdout.lines() {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let Some(&first) = tokens.first() else {
                continue;
            };
            if matches!(first, "default" | "unreachable" | "multicast" | "ff00::/8") {
                continue;
            }
            let mut args: Vec<&str> = Vec::with_capacity(tokens.len().saturating_add(5));
            args.extend(["route", "replace"]);
            args.extend(strip_expires(&tokens));
            args.extend(["dev", dev, "table", GW_TABLE]);
            ctx.ip6_run(&args);
        }
    }
}

/// 安装网关：重建网关路由表（直连路由 + 默认路由经虚拟机），添加全局规则；按形态设置 `IPv6` 默认路由或拦截规则，写 `run/net.gw`。
fn gw_install(ctx: &NetCtx<'_>, gateway: &str, outs: &[String], outs6: &[String]) {
    ctx.ip_run(&["route", "flush", "table", GW_TABLE]);
    let tethers = tether_ifaces(ctx);
    for dev4 in outs.iter().chain(tethers.iter()) {
        copy_dev_routes(ctx, dev4);
    }
    let via4 = format!("{}.254", ctx.cfg.net_prefix());
    let src4 = format!("{}.1", ctx.cfg.net_prefix());
    ctx.ip_run(&[
        "route", "replace", "default", "via", &via4, "dev", XR_IF, "src", &src4, "table", GW_TABLE,
    ]);
    while ctx.ip(&["rule", "del", "pref", GW_PREF]).success() {}
    ctx.ip_run(&["rule", "add", "pref", GW_PREF, "lookup", GW_TABLE]);

    ctx.ip6_run(&["route", "flush", "table", GW_TABLE]);
    for dev6 in outs6.iter().chain(tethers.iter()) {
        copy_dev_routes6(ctx, dev6);
    }
    if gateway == "v6" {
        let via6 = format!("{XR_NET6}::254");
        let src6 = format!("{XR_NET6}::1");
        ctx.ip6_run(&[
            "route", "replace", "default", "via", &via6, "dev", XR_IF, "src", &src6, "table",
            GW_TABLE,
        ]);
    } else {
        ctx.ip6_run(&[
            "route",
            "replace",
            "unreachable",
            "default",
            "table",
            GW_TABLE,
        ]);
    }
    while ctx.ip6(&["rule", "del", "pref", GW_PREF]).success() {}
    ctx.ip6_run(&["rule", "add", "pref", GW_PREF, "lookup", GW_TABLE]);

    while ctx.ip6(&["rule", "del", "pref", REJ6_PREF]).success() {}
    ctx.ip6_run(&["route", "flush", "table", REJ6_TABLE]);
    if gateway == "block" {
        ctx.ip6_run(&[
            "route", "replace", "default", "dev", XR_IF, "table", REJ6_TABLE,
        ]);
        for i in &tethers {
            if i == HS_BR {
                continue;
            }
            ctx.ip6_run(&[
                "rule", "add", "pref", REJ6_PREF, "iif", i, "lookup", REJ6_TABLE,
            ]);
        }
    }
    fsx::write_logged(
        ctx.env,
        ctx.env.run_dir().join("net.gw"),
        format!("{gateway}\n"),
    );
}

/// 拆除网关：删除网关与 `IPv6` 拦截的规则和路由表，删除 `run/net.gw`。
fn gw_remove(ctx: &NetCtx<'_>) {
    while ctx.ip(&["rule", "del", "pref", GW_PREF]).success() {}
    ctx.ip_run(&["route", "flush", "table", GW_TABLE]);
    while ctx.ip6(&["rule", "del", "pref", GW_PREF]).success() {}
    ctx.ip6_run(&["route", "flush", "table", GW_TABLE]);
    while ctx.ip6(&["rule", "del", "pref", REJ6_PREF]).success() {}
    ctx.ip6_run(&["route", "flush", "table", REJ6_TABLE]);
    fsx::remove_logged(ctx.env, ctx.env.run_dir().join("net.gw"));
}

/// 重建 `xr0` 入站的策略路由：先删优先级 18800、18900 的旧规则，上游存在时加回"查本地网络表"和"查上游表"两条。
fn rules_set(ctx: &NetCtx<'_>, use_v6: bool, up: &str) {
    let run_ip = |args: &[&str]| {
        if use_v6 { ctx.ip6(args) } else { ctx.ip(args) }
    };
    while run_ip(&["rule", "del", "pref", "18800"]).success() {}
    while run_ip(&["rule", "del", "pref", "18900"]).success() {}
    if up.is_empty() {
        return;
    }
    let tbl = ctx.lnt();
    note_exec_result(&run_ip(&[
        "rule", "add", "pref", "18800", "iif", XR_IF, "lookup", tbl,
    ]));
    note_exec_result(&run_ip(&[
        "rule", "add", "pref", "18900", "iif", XR_IF, "lookup", up,
    ]));
}

/// 在 `XR_FWD6` 链末尾追加一条 `ip6tables` 规则（`rule` 为 `-A XR_FWD6` 之后的部分）。
fn ip6t_fwd(ctx: &NetCtx<'_>, rule: &[&str]) {
    let mut args = vec!["-A", "XR_FWD6"];
    args.extend_from_slice(rule);
    ctx.ip6t_run(&args);
}

/// `IPv6` 出口段：放行进出 `xr0` 的已建立连接；虚拟机开启 `IPv6` 时放行 `xr0` 到各上游出口。
fn v6_rules_out(ctx: &NetCtx<'_>, outs6: &[String]) {
    ip6t_fwd(
        ctx,
        &[
            "-o",
            XR_IF,
            "-m",
            "conntrack",
            "--ctstate",
            "RELATED,ESTABLISHED",
            "-j",
            "ACCEPT",
        ],
    );
    ip6t_fwd(
        ctx,
        &[
            "-i",
            XR_IF,
            "-m",
            "conntrack",
            "--ctstate",
            "RELATED,ESTABLISHED",
            "-j",
            "ACCEPT",
        ],
    );
    if vm_v6(ctx.env) {
        for out6 in outs6 {
            ip6t_fwd(ctx, &["-i", XR_IF, "-o", out6, "-j", "ACCEPT"]);
        }
    }
}

/// `IPv6` 例外接口段：例外接口可访问虚拟机网段与已生效的 `IPv6` `VPN` 路由；非例外的上游出口到 `xr0` 一律丢弃。
fn v6_rules_ex(ctx: &NetCtx<'_>, outs6: &[String], ex: &[String]) {
    let net6 = format!("{XR_NET6}::/64");
    let active_v6: Vec<String> = fs::read_to_string(ctx.env.run_dir().join("routes.active"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(':'))
        .map(str::to_owned)
        .collect();
    for i in ex {
        ip6t_fwd(ctx, &["-i", i, "-o", XR_IF, "-d", &net6, "-j", "ACCEPT"]);
        for route in &active_v6 {
            ip6t_fwd(ctx, &["-i", i, "-o", XR_IF, "-d", route, "-j", "ACCEPT"]);
        }
    }
    let ex_set: BTreeSet<&str> = ex.iter().map(String::as_str).collect();
    for iface in outs6 {
        if !ex_set.contains(iface.as_str()) {
            ip6t_fwd(ctx, &["-i", iface, "-o", XR_IF, "-j", "DROP"]);
        }
    }
}

/// 拒绝 `from` 到 `to` 的 `IPv6` 转发：`TCP` 回 `tcp-reset`，其余回 `icmp6-adm-prohibited`。
fn ip6t_reject(ctx: &NetCtx<'_>, from: &str, to: &str) {
    let tcp = [
        "-i",
        from,
        "-o",
        to,
        "-p",
        "tcp",
        "-j",
        "REJECT",
        "--reject-with",
        "tcp-reset",
    ];
    ip6t_fwd(ctx, &tcp);
    let other = [
        "-i",
        from,
        "-o",
        to,
        "-j",
        "REJECT",
        "--reject-with",
        "icmp6-adm-prohibited",
    ];
    ip6t_fwd(ctx, &other);
}

/// `IPv6` 网关段：`v6` 模式下共享网络与 `xr0` 双向放行，其余模式拒绝共享网络到 `xr0`；共享网络直连上游出口一律拒绝。
fn v6_rules_gw(ctx: &NetCtx<'_>, gw: &str, outs6: &[String]) {
    if gw == "off" {
        return;
    }
    for i in tether_ifaces(ctx) {
        if gw == "v6" {
            ip6t_fwd(ctx, &["-i", &i, "-o", XR_IF, "-j", "ACCEPT"]);
            ip6t_fwd(ctx, &["-i", XR_IF, "-o", &i, "-j", "ACCEPT"]);
        } else {
            ip6t_reject(ctx, &i, XR_IF);
        }
        for out_if in outs6 {
            ip6t_reject(ctx, &i, out_if);
        }
    }
}

/// 同步 `IPv6`：记录出口与虚拟机 `IPv6` 状态，维护 `XR_FWD6` 转发规则与 nft 回退规则。
fn sync_v6(ctx: &NetCtx<'_>, gw: &str, outs6: &[String], ex: &[String]) {
    let run = ctx.env.run_dir();
    let outs6_str = if outs6.is_empty() {
        String::new()
    } else {
        format!("{} \n", outs6.join(" "))
    };
    fsx::write_logged(ctx.env, run.join("net.up_out6"), outs6_str);
    fsx::write_logged(
        ctx.env,
        run.join("net.v6"),
        if vm_v6(ctx.env) { "1\n" } else { "" },
    );

    ctx.ip6t_run(&["-F", "XR_FWD6"]);
    if acct_present6(ctx) {
        ip6t_fwd(ctx, &["-j", "XR_ACCT6"]);
    }
    hs_fwd6_rules(ctx);
    v6_rules_out(ctx, outs6);
    v6_rules_ex(ctx, outs6, ex);
    v6_rules_gw(ctx, gw, outs6);
    ip6t_fwd(ctx, &["-o", XR_IF, "-j", "DROP"]);
    ip6t_fwd(ctx, &["-i", XR_IF, "-j", "DROP"]);
    fwd6_first(ctx);
    v6_nft_sync(ctx, gw, outs6, ex);
}

/// 列表为空时显示"无"，否则以空格连接。
fn or_none(list: &[String]) -> String {
    if list.is_empty() {
        "无".to_owned()
    } else {
        list.join(" ")
    }
}

/// 本次对账探测到的上游。
#[derive(Debug)]
struct SyncPlan {
    /// 上游接口（无上游时为空）。
    up: String,
    /// `IPv4` 出口接口。
    outs: Vec<String>,
    /// `IPv6` 出口接口。
    outs6: Vec<String>,
}

/// 探测上游接口与出口。
fn detect_upstream(ctx: &NetCtx<'_>) -> SyncPlan {
    let up = upstream_iface(ctx);
    let outs = upstream_out(ctx, &up);
    let outs6 = upstream_out6(ctx, &up);
    SyncPlan { up, outs, outs6 }
}

/// 上游或出口与上次记录不同（或尚无记录）时：更新 `ndc ipfwd` 登记与策略路由规则，写回记录，记日志并在切换时后台抓诊断快照。
fn follow_upstream(ctx: &NetCtx<'_>, plan: &SyncPlan) {
    let (env, cfg) = (ctx.env, ctx.cfg);
    let run = env.run_dir();
    let up = &plan.up;
    let up_if_file = run.join("net.up_if");
    let up_out_file = run.join("net.up_out");
    let old_up = fs::read_to_string(&up_if_file).unwrap_or_default();
    let old_up_trimmed = old_up.trim();
    let old_outs_raw = fs::read_to_string(&up_out_file).unwrap_or_default();
    let outs_joined = if plan.outs.is_empty() {
        String::new()
    } else {
        format!("{} ", plan.outs.join(" "))
    };
    if up == old_up_trimmed
        && outs_joined == old_outs_raw.trim_end_matches('\n')
        && up_if_file.is_file()
    {
        return;
    }
    for old_out in old_outs_raw.split_whitespace() {
        ctx.ndc_run(&["ipfwd", "remove", XR_IF, old_out]);
    }
    if !up.is_empty() {
        for cur_out in &plan.outs {
            ctx.ndc_run(&["ipfwd", "add", XR_IF, cur_out]);
        }
    }
    rules_set(ctx, false, up);
    rules_set(ctx, true, up);
    fsx::write_logged(env, &up_if_file, format!("{up}\n"));
    fsx::write_logged(env, &up_out_file, format!("{outs_joined}\n"));
    let old_disp = if old_up_trimmed.is_empty() {
        "无"
    } else {
        old_up_trimmed
    };
    let new_disp = if up.is_empty() { "无" } else { up.as_str() };
    let outs_disp = or_none(&plan.outs);
    let v6_outs_disp = or_none(&plan.outs6);
    log::info(
        env,
        "net",
        &format!("上游 {old_disp} -> {new_disp}（出口 {outs_disp}）"),
    );
    dlog(
        env,
        cfg,
        "net",
        &format!(
            "上游变化：{old_disp}({old_outs_raw}) -> {new_disp}({outs_disp})，IPv6 出口 {v6_outs_disp}"
        ),
    );
    if !old_up_trimmed.is_empty() {
        trigger_diag_snap_bg(
            env,
            cfg,
            &format!("上游变化 {old_up_trimmed} -> {new_disp}"),
        );
    }
}

/// 在 `XR_FWD` 链末尾追加一条 `iptables` 规则（`rule` 为 `-A XR_FWD` 之后的部分）。
fn ipt_fwd(ctx: &NetCtx<'_>, rule: &[&str]) {
    let mut args = vec!["-A", "XR_FWD"];
    args.extend_from_slice(rule);
    ctx.ipt_run(&args);
}

/// 例外接口可访问虚拟机网段与已生效的 `IPv4` `VPN` 路由；非例外的上游出口到 `xr0` 一律丢弃。
fn fwd_rules_ex(ctx: &NetCtx<'_>, outs: &[String], ex: &[String], net4: &str) {
    let active_v4: Vec<String> = fs::read_to_string(ctx.env.run_dir().join("routes.active"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.contains(':'))
        .map(str::to_owned)
        .collect();
    for i in ex {
        ipt_fwd(ctx, &["-i", i, "-o", XR_IF, "-d", net4, "-j", "ACCEPT"]);
        for route in &active_v4 {
            ipt_fwd(ctx, &["-i", i, "-o", XR_IF, "-d", route, "-j", "ACCEPT"]);
        }
    }
    let ex_set: BTreeSet<&str> = ex.iter().map(String::as_str).collect();
    for iface in outs {
        if !ex_set.contains(iface.as_str()) {
            ipt_fwd(ctx, &["-i", iface, "-o", XR_IF, "-j", "DROP"]);
        }
    }
}

/// 重建 `XR_FWD` 链：计数跳转、已建立连接、`xr0` 到出口、例外接口、网关模式下共享网络与 `xr0` 互通，末尾丢弃其余进出 `xr0` 的流量。
fn apply_forward_rules(ctx: &NetCtx<'_>, outs: &[String], ex: &[String], net4: &str) {
    ctx.ipt_run(&["-F", "XR_FWD"]);
    if acct_present(ctx) {
        ipt_fwd(ctx, &["-j", "XR_ACCT"]);
    }
    let est = [
        "-m",
        "conntrack",
        "--ctstate",
        "RELATED,ESTABLISHED",
        "-j",
        "ACCEPT",
    ];
    ipt_fwd(ctx, &[&["-o", XR_IF][..], &est[..]].concat());
    ipt_fwd(ctx, &[&["-i", XR_IF][..], &est[..]].concat());
    for fwd_out in outs {
        ipt_fwd(ctx, &["-i", XR_IF, "-o", fwd_out, "-j", "ACCEPT"]);
    }
    fwd_rules_ex(ctx, outs, ex, net4);
    if want_gateway(ctx) {
        for i in tether_ifaces(ctx) {
            ipt_fwd(ctx, &["-i", &i, "-o", XR_IF, "-j", "ACCEPT"]);
            ipt_fwd(ctx, &["-i", XR_IF, "-o", &i, "-j", "ACCEPT"]);
            for out_if in outs {
                ipt_fwd(ctx, &["-i", &i, "-o", out_if, "-j", "DROP"]);
            }
        }
    }
    ipt_fwd(ctx, &["-o", XR_IF, "-j", "DROP"]);
    ipt_fwd(ctx, &["-i", XR_IF, "-j", "DROP"]);
    fwd_first(ctx);
}

/// 调试日志记一行本次对账结果；`block` 模式时再记 `IPv6` 拦截说明。
fn log_sync(ctx: &NetCtx<'_>, plan: &SyncPlan, ex: &[String], gw: &str) {
    let (env, cfg) = (ctx.env, ctx.cfg);
    dlog(
        env,
        cfg,
        "net",
        &format!(
            "同步：上游={} 出口={} IPv6出口={} 暴露={} 热点={} 接管={gw}（{}）",
            if plan.up.is_empty() { "无" } else { &plan.up },
            or_none(&plan.outs),
            or_none(&plan.outs6),
            or_none(ex),
            tether_ifaces(ctx).join(" "),
            gw_reason(ctx)
        ),
    );
    if gw == "block" {
        let v6_state = if vm_v6(env) {
            "已开但邻居代理不可用"
        } else {
            "已关闭"
        };
        dlog(
            env,
            cfg,
            "net",
            &format!(
                "IPv6 拦截：虚拟机 IPv6 {v6_state}，手机与热点的 IPv6 出网被拒绝，应用回落 IPv4"
            ),
        );
    }
}

/// 一次完整对账（调用方已持有网络锁）：跟随上游、开放接口、网关形态、路由、流量计数、热点与 `IPv6`，变化时记日志。
fn sync_all_unlocked(env: &Env) {
    let cfg = Config::load(env);
    let ctx = NetCtx::new(env, &cfg);
    let run = env.run_dir();

    let plan = detect_upstream(&ctx);
    follow_upstream(&ctx, &plan);

    let net4 = format!("{}.0/24", cfg.net_prefix());
    ctx.ipt_run(&["-t", "nat", "-F", "XR_NAT"]);
    for nat_out in &plan.outs {
        let rule = [
            "-t",
            "nat",
            "-A",
            "XR_NAT",
            "-s",
            &net4,
            "-o",
            nat_out,
            "-j",
            "MASQUERADE",
        ];
        ctx.ipt_run(&rule);
    }
    mtu_sync(&ctx, &plan.up, &plan.outs);

    let ex = expose_ifaces(&ctx, &plan.up);
    let ex_str = if ex.is_empty() {
        String::new()
    } else {
        format!("{} ", ex.join(" "))
    };
    fsx::write_logged(env, run.join("net.expose_if"), format!("{ex_str}\n"));

    routes_sync(&ctx);
    acct_setup(&ctx, run.join("net.fwd6").is_file());
    apply_forward_rules(&ctx, &plan.outs, &ex, &net4);

    hs_sync(&ctx);
    v6_proxy_sync(&ctx, &plan.outs6);
    let gw = gw_want(&ctx);
    log_sync(&ctx, &plan, &ex, gw);
    sync_v6(&ctx, gw, &plan.outs6, &ex);
    if gw == "off" {
        gw_remove(&ctx);
    } else {
        gw_install(&ctx, gw, &plan.outs, &plan.outs6);
    }
}

/// `xr0` 存在时推送时间并在网络锁内做一次完整对账。
pub(crate) fn sync(env: &Env) {
    if !env.sysnet.join(XR_IF).is_dir() {
        return;
    }
    time_push(env);
    let _lock = net_lock(env);
    sync_all_unlocked(env);
}

/// 创建并配置 `xr0`（`tap`，地址、关闭 `RA` 与反向路径过滤、`IPv6` 地址）并尝试建立热点口；成功返回 `xr0` 的 `IPv4` 地址，创建 `xr0` 失败时返回 `None`。
fn setup_xr0(ctx: &NetCtx<'_>) -> Option<String> {
    let (env, cfg) = (ctx.env, ctx.cfg);
    if !ctx.ip(&["link", "show", XR_IF]).success()
        && !ctx.ip(&["tuntap", "add", XR_IF, "mode", "tap"]).success()
    {
        log::err(env, "net", &format!("创建 {XR_IF} 失败"));
        return None;
    }
    ctx.ip_run(&["addr", "flush", "dev", XR_IF]);
    let ip4_cidr = format!("{}.1/24", cfg.net_prefix());
    ctx.ip_run(&["addr", "add", &ip4_cidr, "dev", XR_IF]);
    for (knob, val) in [
        (format!("sys/net/ipv6/conf/{XR_IF}/disable_ipv6"), "0"),
        (format!("sys/net/ipv6/conf/{XR_IF}/accept_ra"), "0"),
        (format!("sys/net/ipv4/conf/{XR_IF}/rp_filter"), "0"),
    ] {
        ctx.sysw_run(&env.proc_dir.join(knob), val);
    }
    ctx.ip_run(&["link", "set", XR_IF, "up"]);
    let ip6_cidr = format!("{XR_NET6}::1/64");
    ctx.ip6_run(&["addr", "add", &ip6_cidr, "dev", XR_IF, "nodad"]);
    if !hs_tap_up(ctx) {
        // 创建失败已由 hs_tap_up 写入 module.log；xr0 照常继续，热点接管稍后由 sync 重试
        dlog(
            env,
            cfg,
            "net",
            &format!("{HS_TAP} 未就绪，继续启动 {XR_IF}"),
        );
    }
    Some(ip4_cidr)
}

/// 把虚拟机网段登记为本地网络（`ndc` 可用时登记，否则改用 `table 97`）、加 4800 优先级规则并打开 `IPv4` 转发（保存原值）。
fn register_local(ctx: &NetCtx<'_>) {
    let env = ctx.env;
    let run = env.run_dir();
    let net4 = format!("{}.0/24", ctx.cfg.net_prefix());
    let net6 = format!("{XR_NET6}::/64");
    if ctx.ndc_ok(&["network", "interface", "add", "local", XR_IF])
        && ctx.ndc_ok(&["network", "route", "add", "local", XR_IF, &net4])
    {
        fsx::write_logged(env, run.join("net.mode"), "ndc\n");
        ctx.ndc_run(&["network", "route", "add", "local", XR_IF, &net6]);
    } else {
        fsx::write_logged(env, run.join("net.mode"), "fallback\n");
        ctx.ip_run(&["route", "replace", &net4, "dev", XR_IF, "table", "97"]);
        ctx.ip6_run(&["route", "replace", &net6, "dev", XR_IF, "table", "97"]);
        log::info(env, "net", "ndc 不可用，改用 table 97，并由对账循环补回");
    }

    let tbl = ctx.lnt();
    ctx.ip_run(&["rule", "del", "pref", "4800", "to", &net4]);
    ctx.ip_run(&["rule", "add", "pref", "4800", "to", &net4, "lookup", tbl]);
    ctx.ip6_run(&["rule", "del", "pref", "4800", "to", &net6]);
    ctx.ip6_run(&["rule", "add", "pref", "4800", "to", &net6, "lookup", tbl]);

    if !ctx.ndc_ok(&["ipfwd", "enable", "xrouter"]) {
        let fwd_path = env.proc_dir.join("sys/net/ipv4/ip_forward");
        let orig = fs::read_to_string(&fwd_path).unwrap_or_default();
        fsx::write_logged(env, run.join("ip_forward.orig"), orig);
        ctx.sysw_run(&fwd_path, "1");
    }
}

/// 建立 `XR_FWD`、`XR_NAT` 并挂到系统链首；`IPv6` 转发优先用 `ip6tables` 的 `XR_FWD6`，不可用时改用 nft，再挂 `IPv6` `NAT`。
fn create_chains(ctx: &NetCtx<'_>) {
    let env = ctx.env;
    let run = env.run_dir();
    ctx.ipt_run(&["-N", "XR_FWD"]);
    ctx.ipt_run(&["-t", "nat", "-N", "XR_NAT"]);
    fwd_first(ctx);
    nat_first(ctx);
    fsx::remove_logged(env, run.join("net.nat6"));

    if ctx.ip6t(&["-N", "XR_FWD6"]).success() || ctx.ip6t(&["-S", "XR_FWD6"]).success() {
        fsx::touch_logged(env, run.join("net.fwd6"));
        fwd6_first(ctx);
    } else if v6_nft_setup(env) {
        log::info(env, "net", "ip6tables filter 不可用，IPv6 转发改用 nft");
    } else {
        log::err(
            env,
            "net",
            "ip6tables filter 与 nft 均不可用，IPv6 仅靠路由表拦截旁路",
        );
    }
    nat6_first(ctx);
}

/// `xr net up`：创建并配置 `xr0`，打开转发，建立 `XR_FWD`/`XR_NAT` 等链并做首次对账；返回进程退出码（失败为 1）。
#[must_use]
pub(crate) fn up(env: &Env) -> i32 {
    if env.ensure_dirs().is_err() {
        return 1;
    }
    let cfg = Config::load(env);
    let ctx = NetCtx::new(env, &cfg);
    let run = env.run_dir();

    let Some(ip4_cidr) = setup_xr0(&ctx) else {
        return 1;
    };
    register_local(&ctx);
    create_chains(&ctx);
    fsx::remove_logged(env, run.join("net.up_if"));

    {
        let _lock = net_lock(env);
        sync_all_unlocked(env);
    }

    if let Ok(child) = spawn::self_detached(env, &["internal", "net", "watch"]) {
        fsx::write_logged(env, run.join("net_watch.pid"), format!("{}\n", child.id()));
    }
    let mode_s = fs::read_to_string(run.join("net.mode")).unwrap_or_default();
    log::info(
        env,
        "net",
        &format!("xr0 {ip4_cidr} 已就绪（{}）", mode_s.trim()),
    );
    0
}

/// 列表非空时以空格连接并带一个尾随空格（与状态文件的写法一致），空时为空串。
fn joined_trailing(list: &[String]) -> String {
    if list.is_empty() {
        String::new()
    } else {
        format!("{} ", list.join(" "))
    }
}

/// 比对上游、出口、开放接口、网关形态、`IPv6` 出口与来源、路由文件校验和、`MTU` 与上次记录，有变化时记调试日志并完整对账。
///
/// 返回本轮探测到的（上游接口, `IPv4` 出口）。
fn watch_state(ctx: &NetCtx<'_>) -> (String, Vec<String>) {
    let (env, cfg) = (ctx.env, ctx.cfg);
    let run = env.run_dir();
    let up = upstream_iface(ctx);
    let outs = upstream_out(ctx, &up);
    let ex = expose_ifaces(ctx, &up);
    let ex_str = joined_trailing(&ex);
    let outs_str = joined_trailing(&outs);
    let outs6 = upstream_out6(ctx, &up);
    let v6_outs_str = joined_trailing(&outs6);

    let have_gw = fs::read_to_string(run.join("net.gw")).unwrap_or_else(|_| "off".to_owned());
    let gw_trimmed = have_gw.trim();
    let want_gw = gw_want(ctx);
    let ck = routes_file_cksum(ctx);
    let mtu_now = upstream_mtu(ctx, &up, &outs).to_string();

    let read = |name: &str| fs::read_to_string(run.join(name)).unwrap_or_default();
    let old_up = read("net.up_if");
    let old_ex = read("net.expose_if");
    let old_mtu = read("net.mtu");
    let vm_v6_s = if vm_v6(env) { "1" } else { "" };
    let v6src_now = format!("{} ", v6_src_fingerprint(ctx, &outs6));
    let changed = up != old_up.trim()
        || outs_str.trim() != read("net.up_out").trim()
        || ex_str.trim() != old_ex.trim()
        || want_gw != gw_trimmed
        || v6_outs_str.trim() != read("net.up_out6").trim()
        || vm_v6_s != read("net.v6").trim()
        || v6src_now.trim() != read("net.v6src").trim()
        || ck != read("vpn.cksum").trim()
        || mtu_now != old_mtu.trim();
    if changed {
        dlog(
            env,
            cfg,
            "watch",
            &format!(
                "状态变化触发同步：上游 {}->{up} 接管 {gw_trimmed}->{want_gw} 暴露 [{}]->[{ex_str}] MTU {}->{mtu_now}",
                old_up.trim(),
                old_ex.trim(),
                old_mtu.trim()
            ),
        );
        let _lock = net_lock(env);
        sync_all_unlocked(env);
    }
    (up, outs)
}

/// 规则列表 `rules` 中没有以 `<优先级>:` 开头且含 `needle` 的条目时，用 `args`（`rule add pref <优先级> …`）补加；
/// `use_v6` 决定走 `ip` 还是 `ip -6`。
fn ensure_rule(ctx: &NetCtx<'_>, use_v6: bool, rules: &str, needle: &str, args: &[&str]) {
    let head = format!("{}:", args.get(3).copied().unwrap_or_default());
    if rules
        .lines()
        .any(|line| line.starts_with(&head) && line.contains(needle))
    {
        return;
    }
    if use_v6 {
        ctx.ip6_run(args);
    } else {
        ctx.ip_run(args);
    }
}

/// 补回被外部删除的策略路由：4800（虚拟机网段查本地网络表），有上游时再补 18800（`xr0` 入站查本地网络表）与 18900（`xr0` 入站查上游表）。
fn reassert_rules(ctx: &NetCtx<'_>, up: &str, net4: &str, net6: &str) {
    let tbl = ctx.lnt();
    let rules4 = ctx.ip(&["rule"]).stdout;
    let to4 = format!("to {net4}");
    ensure_rule(
        ctx,
        false,
        &rules4,
        &to4,
        &["rule", "add", "pref", "4800", "to", net4, "lookup", tbl],
    );
    let rules6 = ctx.ip6(&["rule"]).stdout;
    let to6 = format!("to {net6}");
    ensure_rule(
        ctx,
        true,
        &rules6,
        &to6,
        &["rule", "add", "pref", "4800", "to", net6, "lookup", tbl],
    );
    if up.is_empty() {
        return;
    }
    let iif = format!("iif {XR_IF}");
    let iif_up = format!("iif {XR_IF} lookup {up}");
    for use_v6 in [false, true] {
        let rules_out = if use_v6 {
            ctx.ip6(&["rule"]).stdout
        } else {
            ctx.ip(&["rule"]).stdout
        };
        ensure_rule(
            ctx,
            use_v6,
            &rules_out,
            &iif,
            &["rule", "add", "pref", "18800", "iif", XR_IF, "lookup", tbl],
        );
        ensure_rule(
            ctx,
            use_v6,
            &rules_out,
            &iif_up,
            &["rule", "add", "pref", "18900", "iif", XR_IF, "lookup", up],
        );
    }
}

/// 回退模式（`net.mode` 为 `fallback`）下补回 `table 97` 中虚拟机网段的路由，并重新下发 `VPN` 路由。
fn reassert_fallback(ctx: &NetCtx<'_>, net4: &str, net6: &str) {
    if !fs::read_to_string(ctx.env.run_dir().join("net.mode"))
        .is_ok_and(|text| text.trim() == "fallback")
    {
        return;
    }
    let head4 = format!("{net4} dev {XR_IF}");
    if !ctx
        .ip(&["route", "show", "table", "97"])
        .stdout
        .lines()
        .any(|row| row.starts_with(&head4))
    {
        ctx.ip_run(&["route", "replace", net4, "dev", XR_IF, "table", "97"]);
    }
    let head6 = format!("{net6} dev {XR_IF}");
    if !ctx
        .ip6(&["route", "show", "table", "97"])
        .stdout
        .lines()
        .any(|line| line.starts_with(&head6))
    {
        ctx.ip6_run(&["route", "replace", net6, "dev", XR_IF, "table", "97"]);
    }
    routes_reassert(ctx);
}

/// 每轮补回挂链钩子、流量计数钩子、`MSS` 规则与热点规则。
fn reassert_hooks(ctx: &NetCtx<'_>, outs: &[String]) {
    fwd_first(ctx);
    nat_first(ctx);
    fwd6_first(ctx);
    nat6_first(ctx);
    acct_hooks(ctx);
    if !ctx
        .ipt(&["-t", "mangle", "-C", "POSTROUTING", "-j", "XR_MSS"])
        .success()
    {
        let _lock = net_lock(ctx.env);
        mss_rules(ctx, outs);
    }
    let _lock = net_lock(ctx.env);
    hs_sync(ctx);
}

/// `xr net watch`：`xr0` 存在期间每 10 秒比对上游、出口、MTU、路由文件等指纹，有变化时完整对账，否则只补齐被外部删除的规则。
#[must_use]
pub(crate) fn watch(env: &Env) -> i32 {
    while env.sysnet.join(XR_IF).is_dir() {
        thread::sleep(Duration::from_secs(10));
        if !env.sysnet.join(XR_IF).is_dir() {
            break;
        }
        let cfg = Config::load(env);
        let ctx = NetCtx::new(env, &cfg);
        time_push(env);
        let (up, outs) = watch_state(&ctx);
        reassert_hooks(&ctx, &outs);
        let net4 = format!("{}.0/24", cfg.net_prefix());
        let net6 = format!("{XR_NET6}::/64");
        reassert_rules(&ctx, &up, &net4, &net6);
        reassert_fallback(&ctx, &net4, &net6);
    }
    0
}

/// 通知对账循环退出；`pid` 文件存在但进程已不在时记一条调试日志。
fn stop_watch(env: &Env, cfg: &Config) {
    if let Ok(w) = fs::read_to_string(env.run_dir().join("net_watch.pid"))
        && !send_signal(&w, rustix::process::Signal::TERM)
    {
        dlog(
            env,
            cfg,
            "net",
            &format!("net_watch（pid {}）已不在，无需停止", w.trim()),
        );
    }
}

/// 摘除并删除 `IPv4` 的 `XR_FWD`/`XR_NAT` 链，并按上次记录的出口撤销 `ndc ipfwd` 转发。
fn drop_v4_chains(ctx: &NetCtx<'_>) {
    while ctx.ipt(&["-D", "FORWARD", "-j", "XR_FWD"]).success() {}
    ctx.ipt_run(&["-F", "XR_FWD"]);
    ctx.ipt_run(&["-X", "XR_FWD"]);
    while ctx
        .ipt(&["-t", "nat", "-D", "POSTROUTING", "-j", "XR_NAT"])
        .success()
    {}
    ctx.ipt_run(&["-t", "nat", "-F", "XR_NAT"]);
    ctx.ipt_run(&["-t", "nat", "-X", "XR_NAT"]);

    if let Ok(old_outs) = fs::read_to_string(ctx.env.run_dir().join("net.up_out")) {
        for out_if in old_outs.split_whitespace() {
            ctx.ndc_run(&["ipfwd", "remove", XR_IF, out_if]);
        }
    }
}

/// 摘除并删除 `IPv6` 的 `XR_FWD6` 链，内核支持 `IPv6` `NAT` 时再删 `XR_NAT6`。
fn drop_v6_chains(ctx: &NetCtx<'_>) {
    while ctx.ip6t(&["-D", "FORWARD", "-j", "XR_FWD6"]).success() {}
    ctx.ip6t_run(&["-F", "XR_FWD6"]);
    ctx.ip6t_run(&["-X", "XR_FWD6"]);
    if !ip6_nat_ok(ctx.env) {
        return;
    }
    while ctx
        .ip6t(&["-t", "nat", "-D", "POSTROUTING", "-j", "XR_NAT6"])
        .success()
    {}
    ctx.ip6t_run(&["-t", "nat", "-F", "XR_NAT6"]);
    ctx.ip6t_run(&["-t", "nat", "-X", "XR_NAT6"]);
}

/// 关闭 `ndc` 转发、恢复 `ip_forward` 原值，撤销本地网络登记、`table 97` 路由并删除 `xr0`。
fn remove_local(ctx: &NetCtx<'_>) {
    let env = ctx.env;
    let run = env.run_dir();
    ctx.ndc_run(&["ipfwd", "disable", "xrouter"]);
    let orig_fwd = run.join("ip_forward.orig");
    if orig_fwd.is_file() {
        let value = fs::read_to_string(&orig_fwd).unwrap_or_default();
        ctx.sysw_run(&env.proc_dir.join("sys/net/ipv4/ip_forward"), value.trim());
    }
    let net4 = format!("{}.0/24", ctx.cfg.net_prefix());
    let net6 = format!("{XR_NET6}::/64");
    ctx.ndc_run(&["network", "route", "remove", "local", XR_IF, &net4]);
    ctx.ndc_run(&["network", "route", "remove", "local", XR_IF, &net6]);
    ctx.ndc_run(&["network", "interface", "remove", "local", XR_IF]);
    ctx.ip_run(&["route", "flush", "table", "97", "dev", XR_IF]);
    ctx.ip6_run(&["route", "flush", "table", "97", "dev", XR_IF]);
    ctx.ip_run(&["link", "del", XR_IF]);
}

/// 删除 `run/net.*`、`routes.active`、`ip_forward.orig` 与 `net_watch.pid`。
fn clear_net_state(env: &Env) {
    let run = env.run_dir();
    if let Ok(entries) = fs::read_dir(&run) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let text = name.to_string_lossy();
            if text.starts_with("net.") {
                fsx::remove_logged(env, entry.path());
            }
        }
    }
    fsx::remove_logged(env, run.join("routes.active"));
    fsx::remove_logged(env, run.join("ip_forward.orig"));
    fsx::remove_logged(env, run.join("net_watch.pid"));
}

/// `xr net down`：停止对账循环，拆除网关、`IPv6` 代理、路由、链与 `xr0`，清理 `run/net.*` 状态文件；返回退出码。
#[must_use]
pub(crate) fn down(env: &Env) -> i32 {
    let cfg = Config::load(env);
    let ctx = NetCtx::new(env, &cfg);

    stop_watch(env, &cfg);
    {
        let _lock = net_lock(env);
        hs_tap_down(&ctx);
    }
    v6_proxy_remove(&ctx, Some("XRouter 网络停止"));
    v6_nft_teardown(env);
    gw_remove(&ctx);
    routes_clear(&ctx);
    drop_v4_chains(&ctx);
    drop_v6_chains(&ctx);
    mss_clear(&ctx);
    acct_teardown(&ctx);

    for pref in ["18800", "18900", "4800"] {
        while ctx.ip(&["rule", "del", "pref", pref]).success() {}
        while ctx.ip6(&["rule", "del", "pref", pref]).success() {}
    }

    remove_local(&ctx);
    clear_net_state(env);
    0
}

/// `xr net status` 输出的网络状态（序列化为 JSON，字段名即 JSON 键）。
#[derive(Debug, Serialize)]
pub(crate) struct StatusJson {
    /// 网络状态 `run/net.mode`，未启动时为 `down`。
    pub(crate) mode: String,
    /// 当前上游接口，未知时为空串。
    pub(crate) up_if: String,
    /// 管理访问开放范围 `XR_EXPOSE`。
    pub(crate) expose: String,
    /// 开放管理访问的接口列表。
    pub(crate) expose_if: Vec<String>,
    /// 虚拟机在 `xr0` 上的地址 `<前缀>.254`。
    pub(crate) vm_ip: String,
    /// 手机在 `xr0` 上的地址 `<前缀>.1`。
    pub(crate) host_ip: String,
    /// 网关形态 `off`、`v6` 或 `block`。
    pub(crate) gw: String,
    /// 被接管的热点接口名，未接管时为空串。
    pub(crate) hotspot: String,
    /// 热点网段（网关地址/前缀），未接管时为空串。
    pub(crate) hotspot_net: String,
}

/// 从 `run/net.*` 状态文件与配置汇总网络状态。
pub(crate) fn status(env: &Env) -> StatusJson {
    let cfg = Config::load(env);
    let run = env.run_dir();
    let mode = fs::read_to_string(run.join("net.mode"))
        .ok()
        .map(|value_text| value_text.trim().to_owned())
        .filter(|content| !content.is_empty())
        .unwrap_or_else(|| "down".to_owned());
    let up_if = fs::read_to_string(run.join("net.up_if"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let expose_if = fs::read_to_string(run.join("net.expose_if"))
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let gw = fs::read_to_string(run.join("net.gw"))
        .ok()
        .map(|raw| raw.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "off".to_owned());
    let hotspot = fs::read_to_string(run.join("net.hs_if"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let hotspot_net = fs::read_to_string(run.join("net.hs_gw"))
        .unwrap_or_default()
        .trim()
        .to_owned();

    StatusJson {
        mode,
        up_if,
        expose: cfg.expose().to_owned(),
        expose_if,
        vm_ip: format!("{}.254", cfg.net_prefix()),
        host_ip: format!("{}.1", cfg.net_prefix()),
        gw,
        hotspot,
        hotspot_net,
    }
}

/// 网络状态的 JSON 文本；序列化失败时返回 `{"mode":"down"}`。
#[must_use]
pub fn status_json(env: &Env) -> String {
    serde_json::to_string(&status(env)).unwrap_or_else(|_| "{\"mode\":\"down\"}".to_owned())
}

/// `xr net` 子命令分发：`up`、`down`、`watch`、`sync`、`status`，其他参数打印用法并返回 2。
pub(crate) fn run_net_cli(env: &Env, args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("up") => up(env),
        Some("down") => down(env),
        Some("watch") => watch(env),
        Some("sync") => {
            sync(env);
            0
        }
        Some("status") => emit(env, &status_json(env), 0),
        _ => emit(env, "usage: net.sh up|down|watch|sync|status", 2),
    }
}
